use std::sync::Arc;

use pumpkin_data::block_properties::{
    BlockProperties, DaylightDetectorLikeProperties, EnumVariants, Integer0To15,
};
use pumpkin_macros::pumpkin_block;
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::{tick::TickPriority, world::BlockFlags};

use crate::{
    block::{
        Block, BlockBehaviour, BlockFuture, EmitsRedstonePowerArgs, GetRedstonePowerArgs,
        NormalUseArgs, OnScheduledTickArgs, PlacedArgs, registry::BlockActionResult,
    },
    world::World,
};

#[pumpkin_block("minecraft:daylight_detector")]
pub struct DaylightDetectorBlock;

/// Calculates the time of day factor (0.0..1.0) based on the given day time, used for brightness and sun angle calculations.
fn time_of_day(day_time: i64) -> f32 {
    let d = ((day_time as f64) / 24000.0 - 0.25).fract();
    let e = 0.5 - (d * std::f64::consts::PI).cos() / 2.0;
    ((d * 2.0 + e) / 3.0) as f32
}

/// Calculates the sky darken value (0..11) based on time of day and weather, matching Java logic.
fn calculate_sky_darken(time: i64, rain_grad: f32, thunder_grad: f32) -> i32 {
    let time_of_day = time_of_day(time);
    let mut sky_darken = 1.0 - ((time_of_day * std::f32::consts::PI * 2.0).cos() * 2.0 + 0.5);

    sky_darken = sky_darken.clamp(0.0, 1.0);
    sky_darken = 1.0 - sky_darken;
    sky_darken *= 1.0 - (rain_grad * 5.0) / 16.0;
    sky_darken *= 1.0 - (thunder_grad * 5.0) / 16.0;
    sky_darken = 1.0 - sky_darken;

    (sky_darken * 11.0) as i32
}

/// Calculates the sun angle (0..2*PI) based on the time of day, used for power calculation when not inverted.
fn get_sun_angle(time: i64) -> f32 {
    time_of_day(time) * std::f32::consts::PI * 2.0
}

/// Calculates the internal light level for a daylight detector at the given position and time.
async fn calculate_internal_light(world: &World, position: &BlockPos, time: i64) -> i32 {
    let sky_light = world
        .level
        .light_engine
        .get_sky_light_level(&world.level, position)
        .await
        .unwrap_or(0);

    let (rain, thunder) = {
        let weather = world.weather.lock().await;
        (weather.rain_level, weather.thunder_level)
    };

    let sky_darken = calculate_sky_darken(time, rain, thunder);
    sky_light as i32 - sky_darken
}

/// Calculates the redstone power level (0..15) based on the internal light level, inverted state, and time of day.
fn calculate_power(internal_light: i32, inverted: bool, time: i64) -> Integer0To15 {
    let signal = if inverted {
        15 - internal_light
    } else if internal_light > 0 {
        let mut sun_angle = get_sun_angle(time);
        let target = if sun_angle < std::f32::consts::PI {
            0.0
        } else {
            std::f32::consts::PI * 2.0
        };
        sun_angle += (target - sun_angle) * 0.2;
        (internal_light as f32 * sun_angle.cos()).round() as i32
    } else {
        0
    };
    Integer0To15::from_index(signal.clamp(0, 15) as u16)
}

/// Recalculates the daylight detector's power level.
/// Only writes the block state if the power actually changed.
async fn update_state(
    world: &Arc<World>,
    position: &BlockPos,
    block: &Block,
    inverted: bool,
    current_power: Integer0To15,
) {
    let time = world.level_time.lock().await.query_daytime();
    let internal_light = calculate_internal_light(world, position, time).await;
    let new_power = calculate_power(internal_light, inverted, time);

    if new_power != current_power {
        let props = DaylightDetectorLikeProperties {
            inverted,
            power: new_power,
        };

        let offset = props.to_index();
        if let Some(new_state) = block.states.get(offset as usize) {
            world
                .set_block_state(position, new_state.id, BlockFlags::NOTIFY_ALL)
                .await;
        }
    }
}

impl BlockBehaviour for DaylightDetectorBlock {
    fn placed<'a>(&'a self, args: PlacedArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            // Only tick in dimensions with skylight
            if args.world.dimension.has_skylight {
                args.world
                    .schedule_block_tick(args.block, *args.position, 20, TickPriority::Normal)
                    .await;
            }
        })
    }

    fn on_scheduled_tick<'a>(&'a self, args: OnScheduledTickArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            let current_state = args.world.get_block_state(args.position).await;
            let props = DaylightDetectorLikeProperties::from_state_id(current_state.id, args.block);

            update_state(
                args.world,
                args.position,
                args.block,
                props.inverted,
                props.power,
            )
            .await;

            args.world
                .schedule_block_tick(args.block, *args.position, 20, TickPriority::Normal)
                .await;
        })
    }

    fn normal_use<'a>(&'a self, args: NormalUseArgs<'a>) -> BlockFuture<'a, BlockActionResult> {
        Box::pin(async move {
            let current_state = args.world.get_block_state(args.position).await;
            let props = DaylightDetectorLikeProperties::from_state_id(current_state.id, args.block);

            // Cycle the inverted property and set with NOTIFY_LISTENERS
            let toggled_props = DaylightDetectorLikeProperties {
                inverted: !props.inverted,
                power: props.power,
            };
            let offset = toggled_props.to_index();
            if let Some(new_state) = args.block.states.get(offset as usize) {
                args.world
                    .set_block_state(args.position, new_state.id, BlockFlags::NOTIFY_LISTENERS)
                    .await;
            }

            // Recalculate power with the new inverted state
            update_state(
                args.world,
                args.position,
                args.block,
                !props.inverted,
                props.power,
            )
            .await;

            BlockActionResult::Success
        })
    }

    fn emits_redstone_power<'a>(&'a self, _: EmitsRedstonePowerArgs<'a>) -> BlockFuture<'a, bool> {
        Box::pin(async move { true })
    }

    fn get_weak_redstone_power<'a>(
        &'a self,
        args: GetRedstonePowerArgs<'a>,
    ) -> BlockFuture<'a, u8> {
        Box::pin(async move {
            let state = args.world.get_block_state(args.position).await;
            let props = DaylightDetectorLikeProperties::from_state_id(state.id, args.block);
            Integer0To15::to_index(&props.power) as u8
        })
    }
}

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

/// Calculates the internal light level for a daylight detector at the given position and time.
async fn calculate_internal_light(world: &World, position: &BlockPos) -> i32 {
    let sky_light = world
        .level
        .light_engine
        .get_sky_light_level(&world.level, position)
        .await
        .unwrap_or(0);

    let sky_darken = world.get_ambient_darkness().await;
    sky_light as i32 - sky_darken
}

/// Calculates the redstone power level (0..15) based on the internal light level, inverted state, and time of day.
fn calculate_power(internal_light: i32, inverted: bool, time_of_day: f32) -> Integer0To15 {
    let signal = if inverted {
        15 - internal_light
    } else if internal_light > 0 {
        let mut sun_angle = time_of_day * 2.0 * std::f32::consts::PI;
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
    let time_of_day = world.get_time_of_day().await;
    let internal_light = calculate_internal_light(world, position).await;
    let new_power = calculate_power(internal_light, inverted, time_of_day);

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

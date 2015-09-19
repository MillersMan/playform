use cgmath::{Point, Vector, Vector3};
use std::ops::Neg;
use std::sync::mpsc::Sender;
use stopwatch;

use common::block_position::BlockPosition;
use common::communicate::ServerToClient::*;
use common::lod::{LOD, OwnerId};
use common::serialize::Copyable;
use common::surroundings_loader::LODChange;

use mob;
use server::Server;
use update_gaia::ServerToGaia;

// TODO: Consider removing the IntervalTimer.

pub fn update_world(
  server: &Server,
  request_block: &Sender<ServerToGaia>,
) {
  let mut request_block = |block| { request_block.send(block).unwrap() };

  stopwatch::time("update", || {
    stopwatch::time("update.player", || {
      for (_, player) in server.players.lock().unwrap().iter_mut() {
        player.update(server, &mut request_block);
      }

      let players: Vec<_> = server.players.lock().unwrap().keys().map(|&x| x).collect();
      for (_, client) in server.clients.lock().unwrap().iter_mut() {
        for &id in &players {
          let bounds = server.physics.lock().unwrap().get_bounds(id).unwrap().clone();
          client.send(UpdatePlayer(Copyable(id), Copyable(bounds)));
        }
      }
    });

    stopwatch::time("update.mobs", || {
      for (_, mob) in server.mobs.lock().unwrap().iter_mut() {
        let block_position = BlockPosition::from_world_position(&mob.position);

        let owner_id = mob.owner_id;
        mob.surroundings_loader.update(
          block_position,
          || { true },
          |lod_change|
            load_placeholders(
              owner_id,
              server,
              &mut request_block,
              lod_change,
            )
        );

        {
          let behavior = mob.behavior;
          (behavior)(server, mob);
        }

        mob.speed = mob.speed - Vector3::new(0.0, 0.1, 0.0 as f32);

        // TODO: This logic is dumb (isolating along components shouldn't be a thing). Change it.
        let delta_p = mob.speed;
        if delta_p.x != 0.0 {
          translate_mob(server, mob, &Vector3::new(delta_p.x, 0.0, 0.0));
        }
        if delta_p.y != 0.0 {
          translate_mob(server, mob, &Vector3::new(0.0, delta_p.y, 0.0));
        }
        if delta_p.z != 0.0 {
          translate_mob(server, mob, &Vector3::new(0.0, 0.0, delta_p.z));
        }
      }
    });

    server.sun.lock().unwrap().update().map(|fraction| {
      for (_, client) in server.clients.lock().unwrap().iter_mut() {
        client.send(UpdateSun(Copyable(fraction)));
      }
    });
  });
}

fn translate_mob(
  server: &Server,
  mob: &mut mob::Mob,
  delta_p: &Vector3<f32>,
) {
  let bounds;
  {
    let mut physics = server.physics.lock().unwrap();
    if physics.translate_misc(mob.entity_id, *delta_p).is_some() {
      mob.speed.add_self_v(&delta_p.neg());
      return;
    } else {
      bounds = physics.get_bounds(mob.entity_id).unwrap().clone();
    }
  }

  mob.position.add_self_v(delta_p);

  for (_, client) in server.clients.lock().unwrap().iter_mut() {
    client.send(
      UpdateMob(Copyable(mob.entity_id), Copyable(bounds.clone()))
    );
  }
}

#[inline]
pub fn load_placeholders<RequestBlock>(
  owner: OwnerId,
  server: &Server,
  request_block: &mut RequestBlock,
  lod_change: LODChange,
) where
  RequestBlock: FnMut(ServerToGaia),
{
  match lod_change {
    LODChange::Load(pos, _) => {
      server.terrain_loader.lock().unwrap().load(
        &server.id_allocator,
        &server.physics,
        &pos,
        LOD::Placeholder,
        owner,
        request_block,
      );
    },
    LODChange::Unload(pos) => {
      server.terrain_loader.lock().unwrap().unload(
        &server.physics,
        &pos,
        owner,
      );
    },
  }
}

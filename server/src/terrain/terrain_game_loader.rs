use common::block_position::BlockPosition;
use common::entity::EntityId;
use common::id_allocator::IdAllocator;
use common::lod::{LOD, LODIndex, OwnerId, LODMap};
use common::stopwatch::TimerSet;
use common::terrain_block::TerrainBlock;
use gaia_thread::{ServerToGaia, LoadReason};
use in_progress_terrain::InProgressTerrain;
use noise::Seed;
use physics::Physics;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use terrain::terrain::Terrain;

/// Load and unload TerrainBlocks from the game.
/// Each TerrainBlock can be owned by a set of owners, each of which can independently request LODs.
/// The maximum LOD requested is the one that is actually loaded.
pub struct TerrainGameLoader {
  pub terrain: Terrain,
  pub in_progress_terrain: InProgressTerrain,
  pub lod_map: LODMap,
}

impl TerrainGameLoader {
  pub fn new() -> TerrainGameLoader {
    TerrainGameLoader {
      terrain: Terrain::new(Seed::new(0), 0),
      in_progress_terrain: InProgressTerrain::new(),
      lod_map: LODMap::new(),
    }
  }

  // TODO: Avoid the double-lookup when unload and load the same index.

  pub fn load(
    &mut self,
    timers: &TimerSet,
    id_allocator: &Mutex<IdAllocator<EntityId>>,
    physics: &Mutex<Physics>,
    block_position: &BlockPosition,
    new_lod: LOD,
    owner: OwnerId,
    ups_to_gaia: &Mutex<Sender<ServerToGaia>>,
  ) {
    let prev_lod;
    let max_lod_changed;
    match self.lod_map.get(block_position, owner) {
      Some((Some(prev), lods)) => {
        prev_lod = Some(prev);
        if new_lod == prev {
          return;
        }

        if new_lod < prev {
          max_lod_changed = lods.iter().filter(|&&(_, l)| l >= prev).count() < 2;
        } else {
          max_lod_changed = lods.iter().filter(|&&(_, l)| l >= new_lod).count() == 0;
        }
      },
      Some((None, lods)) => {
        max_lod_changed = lods.iter().filter(|&&(_, l)| l >= new_lod).count() == 0;
        prev_lod = None;
      },
      None => {
        max_lod_changed = true;
        prev_lod = None;
      },
    }

    if !max_lod_changed {
      // Maximum LOD is unchanged.
      let (_, change) = self.lod_map.insert(*block_position, new_lod, owner);
      assert!(change.is_none());
      return;
    }

    match new_lod {
      LOD::Placeholder => {
        let (_, change) = self.lod_map.insert(*block_position, new_lod, owner);
        let change = change.unwrap();
        assert!(change.loaded == None);
        assert!(prev_lod == None);
        assert!(change.desired == Some(LOD::Placeholder));
        self.in_progress_terrain.insert(id_allocator, physics, block_position);
      },
      LOD::LodIndex(new_lod) => {
        match self.terrain.all_blocks.get(block_position) {
          None => {
            ups_to_gaia.lock().unwrap().send(
              ServerToGaia::Load(*block_position, new_lod, LoadReason::Local(owner))
            ).unwrap();
          },
          Some(mipmesh) => {
            match mipmesh.lods[new_lod.0 as usize].as_ref() {
              None => {
                debug!("{:?} requested from gaia", block_position);
                ups_to_gaia.lock().unwrap().send(
                  ServerToGaia::Load(*block_position, new_lod, LoadReason::Local(owner))
                ).unwrap();
              },
              Some(block) => {
                TerrainGameLoader::insert_block(
                  timers,
                  block,
                  block_position,
                  new_lod,
                  owner,
                  physics,
                  &mut self.lod_map,
                  &mut self.in_progress_terrain,
                );
              },
            }
          }
        };
      },
    };
  }

  pub fn insert_block(
    timers: &TimerSet,
    block: &TerrainBlock,
    position: &BlockPosition,
    lod: LODIndex,
    owner: OwnerId,
    physics: &Mutex<Physics>,
    lod_map: &mut LODMap,
    in_progress_terrain: &mut InProgressTerrain,
  ) {
    let lod = LOD::LodIndex(lod);
    let (_, change) = lod_map.insert(*position, lod, owner);
    // TODO: This should be an unwrap, but the preconditions of another TODO aren't
    // satisfied in src/gaia_thread.rs.
    // (i.e. blocks sometimes get here when they're stale).
    let change = match change {
      None => return,
      Some(change) => change,
    };
    assert!(change.desired == Some(lod));
    change.loaded.map(|loaded_lod|
      match loaded_lod {
        LOD::Placeholder => {
          in_progress_terrain.remove(physics, position);
        }
        LOD::LodIndex(_) => {
          timers.time("terrain_game_loader.load.unload", || {
            let mut physics = physics.lock().unwrap();
            for id in block.ids.iter() {
              physics.remove_terrain(*id);
            }
          });
        },
      }
    );

    timers.time("terrain_game_loader.load.physics", || {
      let mut physics = physics.lock().unwrap();
      for &(ref id, ref bounds) in block.bounds.iter() {
        physics.insert_terrain(*id, bounds.clone());
      }
    });
  }

  pub fn unload(
    &mut self,
    timers: &TimerSet,
    physics: &Mutex<Physics>,
    block_position: &BlockPosition,
    owner: OwnerId,
  ) {
    let (_, mlod_change) =
      self.lod_map.remove(*block_position, owner);

    let lod_change;
    match mlod_change {
      None => {
        return;
      },
      Some(c) => lod_change = c,
    }

    lod_change.loaded.map(|loaded_lod| {
      match loaded_lod {
        LOD::Placeholder => {
          self.in_progress_terrain.remove(physics, block_position);
        }
        LOD::LodIndex(loaded_lod) => {
          timers.time("terrain_game_loader.unload", || {
            match self.terrain.all_blocks.get(block_position) {
              None => {
                // Unloaded before the load request completed.
              },
              Some(block) => {
                match block.lods.get(loaded_lod.0 as usize) {
                  Some(&Some(ref block)) => {
                    let mut physics = physics.lock().unwrap();
                    for id in block.ids.iter() {
                      physics.remove_terrain(*id);
                    }
                  },
                  _ => {
                    // Unloaded before the load request completed.
                  },
                }
              },
            }
          });
        },
      }
    });
  }
}

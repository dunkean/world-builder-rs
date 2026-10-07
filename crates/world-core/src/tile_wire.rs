//! Shared versioned tile transport. WBT2 identifies the current serde Tile
//! schema; changing that schema requires a new magic/version on both ends.

use crate::Tile;
use anyhow::{anyhow, ensure, Result};

pub const MAGIC: &[u8; 4] = b"WBT2";
pub const CONTENT_TYPE: &str = "application/vnd.world-builder.tile+postcard";

pub fn encode_tile(tile: &Tile) -> Result<Vec<u8>> {
    validate(tile)?;
    let payload = postcard::to_allocvec(tile)
        .map_err(|error| anyhow!("cannot encode Postcard tile: {error}"))?;
    let mut bytes = Vec::with_capacity(MAGIC.len() + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

pub fn decode_tile(bytes: &[u8]) -> Result<Tile> {
    ensure!(bytes.starts_with(MAGIC), "invalid tile magic/version");
    let (tile, trailing): (Tile, &[u8]) = postcard::take_from_bytes(&bytes[MAGIC.len()..])
        .map_err(|error| anyhow!("invalid Postcard tile: {error}"))?;
    ensure!(trailing.is_empty(), "unexpected bytes after tile");
    validate(&tile)?;
    Ok(tile)
}

fn validate(tile: &Tile) -> Result<()> {
    ensure!(
        tile.key.face < 6 && tile.key.level <= 24,
        "invalid tile key"
    );
    let count = 1_u32 << tile.key.level;
    ensure!(
        tile.key.x < count && tile.key.y < count && (2..=256).contains(&tile.segments),
        "invalid tile coordinates or grid"
    );
    let segments = tile.segments as usize;
    ensure!(
        tile.vertices.len() == (segments + 1).pow(2)
            && tile.indices.len() == segments * segments * 6,
        "tile grid length mismatch"
    );
    ensure!(
        tile.indices
            .iter()
            .all(|&index| (index as usize) < tile.vertices.len()),
        "tile index outside vertex grid"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TileKey, World, WorldConfig};

    #[test]
    fn round_trip_preserves_float_bits_and_large_water_ids() {
        let world = World::generate(WorldConfig {
            resolution: 8,
            erosion_steps: 2,
            ..Default::default()
        })
        .unwrap();
        let mut tile = world
            .tile(
                TileKey {
                    face: 4,
                    level: 2,
                    x: 1,
                    y: 1,
                },
                16,
            )
            .unwrap();
        tile.vertices[0].water_body_id = Some((1_u64 << 52) + 12345);
        tile.vertices[0].water_level_m = Some(123.25);
        let encoded = encode_tile(&tile).unwrap();
        let decoded = decode_tile(&encoded).unwrap();
        assert_eq!(
            serde_json::to_value(&tile).unwrap(),
            serde_json::to_value(&decoded).unwrap()
        );
        assert_eq!(
            tile.vertices[0].height_m.to_bits(),
            decoded.vertices[0].height_m.to_bits()
        );
        assert_eq!(
            tile.vertices[0].direction.map(f64::to_bits),
            decoded.vertices[0].direction.map(f64::to_bits)
        );
        assert!(encoded.len() * 3 < serde_json::to_vec(&tile).unwrap().len());
    }

    #[test]
    fn truncated_wrong_version_and_trailing_packets_are_rejected() {
        let world = World::generate(WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            ..Default::default()
        })
        .unwrap();
        let tile = world
            .tile(
                TileKey {
                    face: 0,
                    level: 0,
                    x: 0,
                    y: 0,
                },
                2,
            )
            .unwrap();
        let mut encoded = encode_tile(&tile).unwrap();
        assert!(decode_tile(&encoded[..3]).is_err());
        assert!(decode_tile(&encoded[..encoded.len() - 1]).is_err());
        encoded[3] = b'1';
        assert!(decode_tile(&encoded).is_err());
        encoded[3] = b'2';
        encoded.push(0);
        assert!(decode_tile(&encoded).is_err());
    }
}

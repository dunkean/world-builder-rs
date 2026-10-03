//! Pure quadtree partition operations: a displayed cut is an antichain of leaves.
use super::TileKey;
use std::collections::HashSet;

pub(super) fn children(key: TileKey) -> [TileKey; 4] {
    assert!(key.level < 31, "quadtree level exceeds coordinate range");
    std::array::from_fn(|i| TileKey {
        face: key.face,
        level: key.level + 1,
        x: key.x * 2 + (i as u32 % 2),
        y: key.y * 2 + (i as u32 / 2),
    })
}

pub(super) fn parent(key: TileKey) -> Option<TileKey> {
    ancestor(key, key.level.checked_sub(1)?)
}

pub(super) fn ancestor(key: TileKey, level: u8) -> Option<TileKey> {
    if level > key.level || key.level > 31 {
        return None;
    }
    let shift = key.level - level;
    Some(TileKey {
        face: key.face,
        level,
        x: key.x >> shift,
        y: key.y >> shift,
    })
}

/// Reflexive: a leaf covers itself as well as its descendants.
pub(super) fn is_ancestor(coarse: TileKey, fine: TileKey) -> bool {
    ancestor(fine, coarse.level) == Some(coarse)
}

pub(super) fn initial_leaves() -> HashSet<TileKey> {
    (0..6)
        .map(|face| TileKey {
            face,
            level: 0,
            x: 0,
            y: 0,
        })
        .collect()
}

/// Call only after all four children are resident. Failed operations are atomic.
pub(super) fn split(leaves: &mut HashSet<TileKey>, coarse: TileKey) -> bool {
    if !leaves.remove(&coarse) {
        return false;
    }
    leaves.extend(children(coarse));
    true
}

pub(super) fn coarsen(leaves: &mut HashSet<TileKey>, coarse: TileKey) -> bool {
    let quartet = children(coarse);
    if !quartet.iter().all(|key| leaves.contains(key)) {
        return false;
    }
    for key in quartet {
        leaves.remove(&key);
    }
    leaves.insert(coarse);
    true
}

/// Face coordinates use the same half-open ownership rule as streamed tiles.
pub(super) fn covering_key(leaves: &HashSet<TileKey>, face: u8, u: f64, v: f64) -> Option<TileKey> {
    if face >= 6
        || !u.is_finite()
        || !v.is_finite()
        || !(-1.0..=1.0).contains(&u)
        || !(-1.0..=1.0).contains(&v)
    {
        return None;
    }
    for level in (0..=24).rev() {
        let count = 1u32 << level;
        let key = TileKey {
            face,
            level,
            x: (((u + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
            y: (((v + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
        };
        if leaves.contains(&key) {
            return Some(key);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_partition(leaves: &HashSet<TileKey>) {
        for a in leaves {
            for b in leaves {
                assert!(a == b || !is_ancestor(*a, *b));
            }
        }
        for face in 0..6 {
            let area: f64 = leaves
                .iter()
                .filter(|k| k.face == face)
                .map(|k| 4f64.powi(-i32::from(k.level)))
                .sum();
            assert_eq!(area, 1.0, "face {face} must be entirely covered");
            for y in 0..17 {
                for x in 0..17 {
                    let u = 2.0 * f64::from(x) / 16.0 - 1.0;
                    let v = 2.0 * f64::from(y) / 16.0 - 1.0;
                    assert!(covering_key(leaves, face, u, v).is_some());
                }
            }
        }
    }

    #[test]
    fn mixed_cut_covers_each_face_and_coarsens_only_complete_leaf_quartets() {
        let mut leaves = initial_leaves();
        let root = TileKey {
            face: 4,
            ..Default::default()
        };
        assert_partition(&leaves);
        assert!(split(&mut leaves, root));
        let child = children(root)[3];
        assert!(split(&mut leaves, child));
        assert_partition(&leaves);
        let before = leaves.clone();
        assert!(!coarsen(&mut leaves, root));
        assert_eq!(leaves, before);
        assert!(coarsen(&mut leaves, child));
        assert!(coarsen(&mut leaves, root));
        assert_eq!(leaves, initial_leaves());
    }

    #[test]
    fn unordered_child_arrivals_keep_parent_until_all_four_are_ready() {
        let root = TileKey {
            face: 2,
            ..Default::default()
        };
        let quartet = children(root);
        let mut leaves = initial_leaves();
        let mut ready = HashSet::new();
        for (i, index) in [2, 0, 3, 1].into_iter().enumerate() {
            ready.insert(quartet[index]);
            if quartet.iter().all(|k| ready.contains(k)) {
                assert!(split(&mut leaves, root));
            }
            assert_eq!(leaves.contains(&root), i < 3);
            assert_partition(&leaves);
        }
        assert!(coarsen(&mut leaves, root));
        assert_partition(&leaves);
        assert!(split(&mut leaves, root));
        assert_partition(&leaves);
    }

    #[test]
    fn ancestry_and_boundary_ownership_are_unambiguous() {
        let root = TileKey {
            face: 1,
            ..Default::default()
        };
        let child = children(root)[3];
        assert_eq!(parent(root), None);
        assert_eq!(parent(child), Some(root));
        assert_eq!(ancestor(child, 2), None);
        assert!(is_ancestor(root, child));
        assert!(!is_ancestor(child, root));
        let mut leaves = initial_leaves();
        split(&mut leaves, root);
        assert_eq!(covering_key(&leaves, 1, 0.0, 0.0), Some(child));
        assert_eq!(covering_key(&leaves, 1, 1.0, 1.0), Some(child));
        assert_eq!(covering_key(&leaves, 1, f64::NAN, 0.0), None);
        assert_eq!(covering_key(&leaves, 6, 0.0, 0.0), None);
    }
}

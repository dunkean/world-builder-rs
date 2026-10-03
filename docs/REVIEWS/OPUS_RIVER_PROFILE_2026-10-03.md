# Revue CLI Opus 5.5 des profils fluviaux

Rapport réellement obtenu le 3 octobre 2026 avec le modèle confirmé
`claude-opus-5-5`, en lecture seule. Le texte original suit ci-dessous.
La revue porte sur une version intermédiaire du module `river_geometry.rs` ;
les numéros de ligne décrivent cette version et non le fichier final.

Les points 1 et 2 sont corrigés et couverts par des tests d'embouchure et de
sortie lacustre. Le point 3 décrit une tolérance géométrique de 2 mm.
L'inspection ne couvre pas les derniers contrôles secs de déversement et
l'ajustement `2 d / S` du remous. Les résultats de tests et de mesure de la
révision finale figurent dans [RIVER_REVIEW.md](../RIVER_REVIEW.md).

---

I found one blocking bug and two minor edge cases. The rest of `fit_profiles` checks out against your list. I didn't run anything, so this comes from reading the code.

## Blocking

### 1. `clip_mouth` searches one parameter range but saves the result in another (lines 503–504 vs 527–528)

The search walks the raw curve over `[start_parameter, 1]`:
```rust
let point = |t| self.raw_position(self.start_parameter + t * (1.0 - self.start_parameter));
```
But the result is saved as if the search had covered `[start_parameter, end_parameter]`:
```rust
self.end_parameter = self.start_parameter + 0.5*(lo+hi) * (self.end_parameter - self.start_parameter);
```
In `compile` (line 205), every marine or lake-inlet reach reaches `clip_mouth` with `end_parameter` set to the rough estimate from line 136, which is almost always below 1. So the shoreline crossing found at raw `s` gets saved as `s·e` instead of `s`.

- **What goes wrong:** every marine and lake-inlet reach stops early, inland of the real shoreline, at a point where the natural height is still above the fixed level (0 m, or the lake level).
- **How `fit_profiles` makes it worse:** it keeps that end level (lines 281–282, 352–353), and the final pass forces `profile[SEGMENTS] = end_level` (line 366). The water surface therefore drops to sea or lake level on dry ground, and the carved channel cuts down to that level short of the coast.
- **Why your probe didn't catch it:** this puts water *below* the natural ground, so "no water >10 m above natural" passes, and channel depth is unchanged.
- **Your own tests should fail:** in `marine_endpoint_uses_zero_level_and_stops_before_the_seabed` the estimate is 0.25 and the crossing is at about 0.25. The end ends up near 0.0625, where the height is about 75, so `height(channel.end).abs() < 1e-6` (line 735) fails. `lake_inlet_ends_at_the_reservoir_shore…` fails the same way: end near 0.25, height about 125, expected 100 (line 762). Running `cargo test -p world-core river_geometry` will confirm or refute this quickly.
- **Why the clipping inside `fit_profiles` is safe:** at line 317 the reach is always non-terminal, so `end_parameter == 1` (line 139) and the two ranges happen to match.

**Minimal fix (line 527–528):**
```rust
self.end_parameter = self.start_parameter + 0.5 * (lo + hi) * (1.0 - self.start_parameter);
```
After this, numerical512 will move the marine and lake-inlet endpoints, so it should be re-run.

## Minor, not blocking

### 2. `clip_outlet` ignores an already-clipped `end_parameter` (lines 541–560, called at 207–208)

`compile` calls `clip_mouth` first and `clip_outlet` second. `clip_outlet` searches raw `t ∈ [0, 1]` and assigns `start_parameter` directly.

- **When it bites:** a reach that both leaves a lake and enters the sea or another lake, where the reconstructed ground drops below the downstream level and then rises above the upstream lake level further on.
- **Effect:** `start_parameter` can end up at or past `end_parameter`. The reach then runs backwards or collapses to a point. Nothing panics: `derivatives` with zero span falls back to bisection in `closest`.
- **Minimal fix:** limit the search to the live range. Use `self.raw_position(t * self.end_parameter)` at lines 543 and 554, and `self.start_parameter = 0.5 * (lo + hi) * self.end_parameter;` at line 560. This works whatever order the two clips run in.

### 3. A reach can be flagged as a mouth without being shortened (lines 313–318)

If the ground at the start of a reach is in `[floor − 0.002, floor)`, it passes the removal check at line 302. It also counts as an intersection at sample 0. Lines 314–316 then mark it as a mouth with `end_level = floor`, but `clip_mouth` returns early at line 500 because `height(start) <= end_level`.

The reach stays full length, ending at its downstream cell at the floor level. That cell is then removed at line 303, since its ground is lower still. The water sits at most about 2 mm off, so this is harmless. Starting the intersection check at sample 1 would avoid the case if you want it gone.

## Checked and correct

- **Acyclic graph and cycles:** Kahn's algorithm only queues reaches that exist. The indegree counts go up (247) and down (264) under the same non-terminal condition, so the `u32` can't underflow. After the loop, any reach with indegree ≠ 0 is removed (272–276). In a one-receiver-per-cell graph nothing sits downstream of a cycle. Self-loops and two-cell loops are dropped. Every `unwrap` at 262, 280, 301 and 334 only sees reaches that exist.
- **Shared nodes:** upstream `end_level = levels[to]` is written in the last loop (355), after `to` has had its own `max(floors)` raise (335). `floors[to] == floors[i]` for non-terminal reaches. Downstream `profile[0] = start_level = levels[to]`, so `level(1)` upstream equals `level(0)` downstream exactly. Widths and depths match because both ends use the same `depth(max(next.discharge, q))`.
- **Fixed marine and lake levels:** the end level of a terminal reach is never refitted (352–353). For terminal reaches the floor equals the end level, so `start ≥ end`. Lake outlet starts are pinned (330–332) and can only be lowered by non-terminal reaches flowing into the control cell. That can't happen, because the control cell is a lake cell and anything entering a lake cell is a lake inlet (line 130; checked by `hydrology.rs:515`).
- **Natural-height ceiling:** the fitted values stay at or below the running minimum of `max(natural + 0.35·depth, floor)`. Taking the minimum of `max(v, f)` (final loop) equals `max(min v, f)` (fit loop), so the two passes agree. The floor can only raise a sample where the ground is below the floor, and the clipping removes that case up to the 1e-6 / 0.002 tolerances.
- **Can a newly clipped upstream reach leave a downstream level undefined? No.**
  - After clipping, `levels` is rebuilt from scratch (322–332). Every reach still in `order` gets a finite value from its own `profile[0]` (325).
  - A downstream reach whose only upstream was clipped starts from its own profile, raised to the floor. That's defined, just with a dry gap by design.
  - A non-reach end cell gets a finite value from line 327 and the line 346 minimum.
  - A reach removed at 303 can't have a surviving unclipped upstream feeding it. The upstream's last sample is exactly the removed reach's start (`raw_end == next.position`; it's not an outlet, since the control cell is a lake cell). That ground is below `floor − 0.002`, which is below `floor − 1e-6`, so line 312 always marks the upstream as terminal first.
  - `floors` stays at −∞ only for chains that never reach a terminal. Those skip clipping, and `max(−∞)` does nothing. No infinity leaks into a saved level.
- **Monotone Hermite interpolation:** knot slopes use the harmonic mean, zero when the neighbouring steps have opposite signs, which keeps each slope within twice the step. The end segments use the step itself. That satisfies the Fritsch–Carlson monotonicity condition, so the curve stays between its two knots. `t == 1` returns `end_level`. For `t < 1`, `floor(t·128) ≤ 127`, so `i + 1` and `i + 2` are always in range. The division in `slope` only happens when `a·b > 0`, so it never divides by zero.
- **Lake outlet parameter mapping:** `position`, `derivatives`, profile sampling and `closest` all use the same normalised `t` over `[start_p, end_p]`. Clipping inside `fit_profiles` keeps the two ranges consistent because `end_p` is 1 there. The only mismatches are #1 and #2 above.

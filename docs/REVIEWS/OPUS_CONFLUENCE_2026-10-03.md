# Revue CLI Opus 5.5 des confluences

Revue intermédiaire en lecture seule du 3 octobre 2026, modèle effectif `claude-opus-5-5`, CLI 2.1.288. Le texte ci-dessous précède l’implémentation et ses tests ; ses hypothèses ne sont pas des garanties. Voir [le rapport fluvial](../RIVER_REVIEW.md) pour les mesures et corrections finales.

I've finished reading the code and the probe output. Below is the diagnosis and the fix strategy, with evidence and hypotheses kept separate. No files were edited.

# Review: river overlap terrain/water jumps

## Evidence (from the probe JSON and the code)

**The IDs.** `channel_at` sets `id = RIVER_ID_BASE + index + 1` (`sampler.rs:591`). So body `…493164` is reach **1122667** and body `…493165` is reach **1122668**.

**The reaches.** `mismatch_cases` gives `from 1122668 → to 1123180`, which is one row down (+512). Reach 1122667 is the neighbour in the same row (−1), not 668's receiver.

**667's level is constant.** At every bisected boundary, 667's side reads exactly `118.42862701416016` (cases 2–5, all cross offsets, t = .78–.875). `river_after` is about 1e‑10 there, so each point sits right on 667's half‑width circle. At that circle `bank_height` returns exactly `water_level_m` (`river_geometry.rs:753-759`). So 667's level along this stretch is a single constant.

**668's water heads to the same value.** 668's own levels are 122.67 / 120.55 at t = .9375 / .96875. That is about −2.12 m per 1/32, which extrapolates to about **118.43 at t = 1**. So `levels[1123180]` almost certainly equals 667's level, which means the end node itself matches.

**How the jump happens (`sampler.rs:105-131`).**
1. Line 105 sets `height = min(carved)` over all reaches. Near 667's edge that is about 118.4, and it is continuous.
2. The owner rule (lines 110-117) prefers the wet reach. Inside 667's half‑width both reaches are wet, and 667 wins because its bed is lower. Just outside, 667 is dry and 668 wins.
3. Line 129 then replaces the height with `natural.min(bed_668)`, which is about 132–138 m. That throws away 667's carved valley.
4. Result: a 14–20 m terrain step exactly at 667's half‑width. The wet→dry flag flips at the same place.

**The underlying conflict.** Two wet sections overlap while their water levels differ by up to 20 m. In that case no terrain field can be both continuous and show each owner's water correctly. The override doesn't cause this; it only moves the step. That matches what you saw: removing the override restores continuity but brings back the floating water.

**Why 668 sits high.** The profile ceiling is `natural(centreline) + 0.35·depth`, followed by a running min (`river_geometry.rs:302`, `441-466`). Nothing in the fit can see another reach's carved valley. So 668 stays on its high natural ground and only ramps down to the shared node level in the last stretch.

## Hypotheses (not yet proven)

- **H1 (more likely): two converging siblings, not an unrelated endcap.** 667 probably also drains to 1123180, and the two curves converge so their wet sections overlap over the last ~20% of 668. 667's constant level would then be a flat stretch of its profile, where the running min hit low ground early. This fits the "tributary profile not adjusted to the receiving incision" idea, applied to a sibling or receiver.
- **H2: an endcap.** A constant level is also what you'd see if 667's closest point is clamped at t = 0 or 1. That could come from a reach dropped at `river_geometry.rs:383-385` (`channels[i] = None`), which cuts the network and leaves a dangling upstream end. I can't rule it out from this JSON.
- **The 1.098 m wet/wet case (1116579 vs 1117602).** 1117602's level is also constant (313.952) at both boundaries, so it is the same kind of case (H1 or H2). 1117602 = 1117090 + 512, so it may be the next node downstream from 1116579's receiver. Unconfirmed.
- **How to settle it:** have `quality_probe` print the other owner's `from/to`, its closest `t` (exactly 0/1 means endcap), and `cells[667].receiver`.

The fix below works under both H1 and H2.

## Minimal strategy: fit each reach's profile against the other reaches' terrain

**1. Factor out the per-candidate carve.** Pull `sampler.rs:81-104` into a shared function, e.g. `carve(candidate, natural) -> f64`. It is used in two places: the sampler, and a new helper for the profile fit:
- `surface_B(p) = carved_B(p)`, except inside B's wet half‑width, where it is `max(carved_B, B.level(t))`.
- Using B's water level (not its bed) inside the wet zone stops two reaches from ratcheting each other's levels down.

**2. Add a cap pass after the final profile loop** (after `river_geometry.rs:470`, still inside `fit_profiles_with_controls`, so the result stays compiled once and immutable). For each reach A, at each interior sample j = 1..SEGMENTS‑1:
- Compute `cap_j = min over B ≠ A of surface_B` at three points: A's centreline and both wet edges (centreline ± half‑width along the normal).
  - Excluding A itself removes the own‑incision feedback.
  - Including the receiver and upstream reaches is intended: their ends sit at the shared node level, so they cap nothing there.
  - Find candidate B's with the same neighbour/chord pre‑check `channel_at` uses (`sampler.rs:558-583`).
- Set `profile[j] = min(profile[j], max(cap_j, floors[i], end_level_m))`, then redo the running min.
- Never touch `profile[0]`, `profile[SEGMENTS]`, the lake/marine/spill controls, or `start/end_controlled`. Nodes and confluences therefore still match exactly.
- Run 2–3 passes in `order`. The cap only ever lowers values, so it converges. With symmetric caps, levels in a wet/wet overlap end up equal; each sits at or below the other's surface.

**3. Remove the binary override** (`sampler.rs:122-131`). Terrain goes back to the continuous `min(carved)` from line 105.
- **Wet edge:** at A's half‑width, `carved_A` equals `level_A`, and the cap keeps `surface_B ≥ level_A` there, so water depth fades continuously to 0.
- **Wet/wet:** keep the owner selection. Its levels now agree, so switching owner causes no step.

**4. Residual case to report, not hide.** If `cap_j` would have to go below A's own end node level, B is an unrelated, lower river. Don't add a dam or a mask. Count these in the probe. Only if they occur should a follow‑up lower `levels[to]` through the existing propagation (`river_geometry.rs:417-431`).

**Cost.** About 10k reaches × 127 samples × 3 points × a few neighbours, each needing a Newton `closest()`. I'd expect that to add hundreds of ms to the catalogue compile, which currently takes 0.66 s. Measure it.

## Tests

- **Profile cap (`river_geometry.rs` tests):** extend the `fitted_water_follows_reconstructed_valley…` setup with two reaches converging on the same node, one of them perched on a ridge 15 m higher. Assert:
  - |level_A − level_B| < 5 cm at every A sample inside B's wet zone;
  - `profile[0]` and `profile[SEGMENTS]` are unchanged;
  - A's profile never rises downstream (monotone non‑increasing);
  - the existing `fitted_profile_preserves_fixed_marine_and_reservoir_controls` still passes.
- **Wet/dry (sampler):** bisect across B's half‑width while inside A's wet section (the probe's layout). Assert Δheight < 1 cm per mm, Δwater < 1 cm, and water depth < 1 cm at the transition.
- **Wet/wet (sampler):** at an owner switch, assert Δwater < 5 cm.
- **Keep what the override was protecting:** wherever A is wet above B's dry valley, assert `water − height ≤ depth_A + ε`, so no water floats 20 m up.
- **Full probe** (atlas 512, seed 2386084681). Targets:
  - `height_jumps_over_10cm = 0`
  - `owner_switch_jumps_over_10cm = 0`
  - `non_lake_non_ocean_profile_mismatches` drops to 0, or every remaining case is explained by an owner `t` the probe prints
  - `max_river_water_depth_m` and `water_more_than_10m_above_natural_terrain` no worse than now
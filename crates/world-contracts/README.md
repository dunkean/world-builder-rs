# Conservative refinement prototype

`refine(&Grid, seed, amplitude_m)` produces four equally sized children in
NW, NE, SW, SE order. `Grid.size` counts **vertices**, not cells. Supply
`(size-1)²` positive cell areas, preferably measured on the sphere. For existing
callers, `size²` positive nodal metric samples are averaged into cell areas.

The reference surface uses bilinear interpolation inside each parent cell.
Its area is split equally among four fine cells; this is a **piecewise constant
metric approximation**, not exact spherical quadrature. New internal edge
midpoints receive deterministic noise; each cell center compensates that noise
to preserve the parent's cell altitude mean. The complete family is compiled
before its four overlapping vertex grids are extracted.

Inherited vertices and outer edges stay fixed. Global first moments of the
residual are projected when their two-direction system has full rank. All
values are quantized on the same signed 32-bit centimetre grid, and the
certificate is computed from the decoded children. Mean errors are bounded by
half a quantization step; no averaging repairs mismatched shared edges.

The amplitude bounds geometric detail relative to the inherited interpolation.
This prototype does **not** constrain river receivers, preserve coast masks,
conserve sediment mass, or provide refined interface detail. Those additional
contracts require the corresponding hydrological plans and inequalities. The
certificate explicitly reports `drainage_constrained: false`.

Run `cargo test -p world-contracts`. The tests exercise weighted local means,
independently generated neighboring boundaries, quantized anchors, deterministic
regeneration and rejection of corrupted seams.

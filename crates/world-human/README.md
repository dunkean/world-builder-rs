# Agrarian human layer

`HumanLayer::generate(&world, HumanConfig::default())` places villages and towns
on dry atlas cells with river, lake or moist coastal access. Suitability combines
temperature, moisture, lithology and slope; a metric minimum distance separates
settlements. The integer population budget is allocated exactly and any absence
of suitable land is reported as `unplaced_population`.

Roads join each inhabited land component with a geodesic minimum spanning tree,
then use deterministic eight-neighbor A* with slope and river-crossing costs.
Routes avoid ocean/lake cells and corner cutting; grades above 25% are forbidden.
An impossible route is counted in `unreachable_roads`. `island_components`
reports inhabited components without inventing ferry connections.

This prototype uses atlas elevations. It does not reserve road corridors in
fine terrain, construct bridges, simulate drinking-water extraction or enforce
agricultural carrying capacity. Moist coastal access is a groundwater/rainfall
proxy, not freshwater supplied by the sea. Route altitudes are suitable for an
atlas overlay; continuous detailed terrain may differ between those samples.

Run `cargo test -p world-human` to check stable identities, population accounting,
settlement spacing, dry road paths and behavior without habitable land.

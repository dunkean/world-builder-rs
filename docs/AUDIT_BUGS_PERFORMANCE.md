# Audit bugs et performance — topologie côtière, LOD, streaming et mémoire

**Modèle** : Sonnet 5.5 (`claude-sonnet-5-5`), agent indépendant en lecture seule.
**Dépôt** : `/home/dunkean/projects/world_builder_rs`, branche `main`, commit `99574c3`, arbre propre au démarrage.
**Fichier cible** : `docs/AUDIT_BUGS_PERFORMANCE.md`. Je n'ai rien écrit sur disque, c'est à root de l'enregistrer.

## 0. Méthode et limites

Je n'avais que Read, Glob et Grep. **Je n'ai rien exécuté** : ni test, ni serveur, ni navigateur, ni GPU. Chaque constat porte donc une étiquette :

- **[CODE]** : lu directement dans le code actuel, avec `fichier:ligne`.
- **[DEP]** : lu dans les sources d'une dépendance (`wgpu-27.0.1` dans `~/.cargo/registry`).
- **[DOC]** : chiffre déjà publié dans `docs/IMPLEMENTATION.md`, non remesuré ici.
- **[INFÉRENCE]** : déduction ou arithmétique à partir des constantes du code.
- **Rien n'est « reproduit »** au sens strict. Aucune validation GPU n'est revendiquée : le comportement du shader et du navigateur n'est qu'inféré.

**Lu en entier** : `world-core` (`lib.rs`, `sampler.rs`, `hydrology.rs`, `regions.rs`, `geometry.rs`, `tile_wire.rs`), `world-viewer` (`lib.rs` hors tests, `lod.rs`, `shader.wgsl`), `web/*.js`, `preview.rs`, `hierarchy.rs` et la route `/tiles` de `main.rs`.
**Non examiné** : `erosion.rs`, `world-erosion`, `world-human`, `families.rs`, `regions.rs` côté app, `index.html`, CUDA. Ces zones appartiennent aux autres agents.

## 1. Résumé

| # | Sév. | Constat | Statut |
|---|---|---|---|
| C1 | **Critique** | L'océan est décidé par `hauteur_fine < 0`, sans connectivité. Le détail fractal (jusqu'à ~170 m) n'est pas borné près du niveau de la mer. | [CODE] + [DOC] |
| C2 | **Haute** | Le fractal est échantillonné à fréquences globales fixes, sans lien avec l'espacement de la tuile : crénelage et topologie différente à chaque LOD. | [CODE] + calcul |
| C3 | **Haute** | Le plafond d'amplitude lacustre saute au bord d'un carré 2×2 cellules d'atlas : falaises en carrés. | [CODE] |
| P1 | **Critique** | Le scheduler de coupe est sériel : un split ou une fusion à la fois, un job par image, tranche de 4 ms, morph de 0,35 s. | [CODE] + [INFÉRENCE] |
| P2 | **Haute** | Les ancêtres ne sont pas épinglés. Au dézoom ils sont évincés puis redemandés. L'état `loaded` côté JS peut devenir périmé. | [CODE] |
| P3 | **Haute** | Mémoire par tuile ≈ 9 Mio CPU + 6–7 Mio GPU. Le budget de 768 Mio tient ~85 tuiles. | [CODE] + [INFÉRENCE] |
| P4 | **Haute** | `Drop` d'un buffer WebGPU est un no-op dans wgpu 27.0.1, et `destroy()` n'est jamais appelé. | [DEP] + [CODE] |
| C4 | Moyenne | Selles de marching-squares : connectivité 8 de l'atlas contre champ continu, diagonale fixe. | [CODE] + [INFÉRENCE] |
| C5 | Moyenne | Bruit de valeur sur réseau aligné aux axes avec smoothstep : motif en carrés à fort zoom. | [INFÉRENCE] |
| P5–P9 | Moyenne | Installation sur le thread principal, clones inutiles, sondes `covering_key`, JS (annulations, démarrage), serveur (scan disque). | [CODE] |
| C6–C8, P10–P11 | Basse | Voir §5. | |

**Des causes déjà écartées, avec preuve** :
- Les « nombreux petits lacs côtiers » ne viennent probablement pas de l'hydrologie de l'atlas. `docs/IMPLEMENTATION.md:120` rapporte 16 lacs et 90 cellules mouillées pour le monde de référence (graine 1337). Cela reste à confirmer sur le monde de l'utilisateur via `stats.lake_count` et `stats.lake_cells`.
- Le calcul serveur n'est pas le goulot de la convergence : p50 ≈ 11 ms pour 64 segments (`:120`) et 34–38 → 11–12 ms par tuile (`:55`), soit ~45 ms estimés pour 128 segments. [DOC] + [INFÉRENCE]

## 2. Topologie côtière et LOD (racine)

### C1 — Critique — La côte est le niveau zéro d'un champ fractal non borné

**Où** :
- `crates/world-core/src/sampler.rs:123-131` : amplitude = `detail_m × clamp(1.3 − 0.4·érodabilité, 0.3, 1.4)`. Soit 81 à 171 m pour `detail_m = 140`.
- `sampler.rs:124-128` : le plafond près de l'eau ne s'applique que si `id != 0`, donc jamais à l'océan.
- `sampler.rs:153-170` : `height < 0 && (pas d'eau || océan)` met `ocean = true`, niveau 0, sans test de connexion à l'océan global (ni à une cellule `ocean`).

**Cause racine** : `height = patch.height + fractal × amplitude`. Dans la bande où `|patch.height| ≲ 100 m`, le fractal change le signe et crée des composantes isolées. C'est la zone côtière sur ~1–2 cellules d'atlas.
- **Mares** : de l'« océan » apparaît en terre sans lien avec la mer, d'où les petits lacs côtiers.
- **Îles** : des îlots apparaissent en mer, d'où les péninsules coupées.
- **Baies fermées** : un seuil ou un banc de sable traverse l'embouchure.

**Indices concordants** :
- Le test `fractal_coast_is_stable…` (`sampler.rs:881-886`) ne vérifie la stabilité du signe que pour `|h| > 80 m`. Il reconnaît donc la bande instable, plus étroite que l'amplitude réelle (jusqu'à 171 m).
- La doc `:124` ne valide visuellement qu'une seule côte.

**Statut** : [CODE] pour le mécanisme, [INFÉRENCE] pour l'ampleur (aucun rendu ni mesure).

**Solution minimale** (dans `sample`, après le calcul de `amplitude`) :
1. Borner l'enveloppe par la distance au niveau de la mer du champ lisse LOD-invariant : `amplitude = amplitude.min(0.5·|patch.height| + ε)`, avec ε ≈ 2–5 m. Le contour zéro suit alors le contour de `patch.height`, déterminé par l'atlas et son océan connecté, quel que soit le LOD.
2. Ne passer en `ocean = true` (§153) que si `patch.height < 0` ou si un datum océan existe. Un point inland sous zéro sans propriétaire de bassin serait alors un bassin sec, pas une mer.
3. Si l'on veut de la rugosité côtière, la faire en déplacement horizontal borné (domain-warp), jamais en hauteur.

Les invariants existants (égalité de bits des sommets partagés) sont conservés, car l'amplitude reste fonction de la position seule.

**Tests** :
- **T1** : monde 128², fenêtre côtière ~200 km, grilles de `world.sample` aux niveaux L2…L10. Masque `ocean`, flood-fill depuis une cellule océan connue. Vérifier que le nombre de composantes ne croît pas avec L et qu'aucun « océan » n'est non connecté.
- **Mesure avant/après** : `#composantes_océan(L)` et `#îles(L)` par niveau. Avant correction, attendre une courbe non monotone.

### C2 — Haute — Fractal non limité en bande selon le LOD

**Où** :
- `sampler.rs:468-500` : fréquence de base `résolution × 0,75` (=96 pour n=128) et jusqu'à 16 bandes (`rayon/fréquence < 0,5`).
- `crates/world-viewer/src/lib.rs:2100-2114` : tuiles toujours à 128 segments.
- `app.js:233` : `segments=128`.

**Cause racine** : `sample()` ne connaît pas l'espacement de la tuile.
- La bande 0 a une période ≈ 18 km. L'espacement des sommets à L0 est ≈ 21 km.
- Il faut un espacement ≤ période/2, donc la bande *k* n'est résolue qu'à partir de L ≈ k+2. Le champ est donc sous-échantillonné aux niveaux bas.
- Chaque LOD « révèle » une nouvelle bande. Le contour change à chaque raffinement.
- Coût secondaire : 19 évaluations de bruit (3 de warp + 16 bandes, ≈150 hash64) par sommet même à L0. [INFÉRENCE]

**Statut** : [CODE] pour les constantes, [INFÉRENCE] pour le calcul de Nyquist.

**Solution minimale** :
- **Option A (recommandée d'abord)** : appliquer C1. La topologie côtière ne dépend plus des bandes fines, et le crénelage ne reste que visuel hors côte.
- **Option B** : ajouter `sample_lod(p, espacement_m)` qui fait décroître les bandes de longueur d'onde < 2 × espacement, et que `tile()` appelle. Cela casse l'égalité de bits parent/enfant. Il faudrait la remplacer par une égalité aux sommets partagés *à bandes communes*, et laisser le morph (`parent_source`) absorber l'écart. À décider avec le propriétaire du viewer.

**Test** : sur 1 000 points aléatoires côtiers, |h(L) − h(L+1)| pour des sommets partagés. Borne ≤ amplitude de la première bande non résolue.

### C3 — Haute — Falaises en carrés autour des lacs

**Où** : `sampler.rs:124-128` (plafond d'amplitude) et `sampler.rs:370-414`, en particulier `377-388` (un lac n'est candidat que s'il est un coin du quad avec poids > 1e-10).

**Cause racine** : `shoreline_datum` n'existe que dans les 4 quads qui entourent une cellule lac, soit un carré 2×2 cellules ≈ 42 km. Dans ce carré, l'amplitude est plafonnée à `3 + 0,12·|Δ|` (quelques mètres). Juste en dehors, elle est de 80–170 m. La hauteur saute de `fractal × (A_dehors − A_dedans)`, soit potentiellement des dizaines de mètres, le long de lignes alignées sur la grille de l'atlas.

**Statut** : [CODE] pour la discontinuité. [INFÉRENCE] pour le lien avec les « carrés à fort zoom » (non rendu).

**Solution minimale** : remplacer le plafond binaire par une fonction continue de la distance au lac. Par exemple, pondérer par `smooth(dist / (1,5·cellule))` sur la distance au centre de la cellule lac la plus proche, ou plafonner via `|patch.height − niveau|` pour tout point dont `patch.height` est proche d'un niveau de lac connu. Ne pas dépendre de `weight > 1e-10`.

**Test T2** : traverser à pas de 1 m les 4 frontières d'un quad entourant un lac (balayage perpendiculaire). Exiger |Δh| ≤ pente_max × 1 m + 0,05 m. Étendre ce test de continuité à toutes les frontières de quad et de faces de cube.

### C4 — Moyenne — Selles et connectivité 8

**Où** :
- `hydrology.rs:123-129` : l'océan atlas est inondé en 8-voisinage (`neighbours8`).
- `sampler.rs:320-325` : la reconstruction est un cubique monotone par lignes puis par colonnes, avec moyenne.
- `sampler.rs:246-256` et `viewer lib.rs:1041` : diagonale fixe `a→a+s+2` dans chaque quad.

**Cause racine** : deux cellules océan en diagonale, avec deux cellules terre en antidiagonale, sont connectées côté atlas. Le champ continu peut séparer la selle dans les deux sens. À L0 la diagonale du maillage tranche. À un LOD plus fin, c'est le champ qui décide. Une baie ou un détroit « diagonal » peut donc se fermer, ou une péninsule diagonale se couper, au raffinement.

**Statut** : [CODE] pour les deux conventions, [INFÉRENCE] pour l'effet.

**Solution minimale** : mesurer d'abord la fréquence (**T3** : nombre de quads atlas de motif ABBA par monde, comparer au nombre d'îles apparues). Si significatif, passer l'inondation océan en 4-voisinage et traiter les terres en 8-voisinage (dualité 4/8), ou imposer le signe au centre des selles.

### C5 — Moyenne — Motif en carrés à fort zoom (non reproduit)

**Hypothèses classées**, par ordre de probabilité :
1. **C3** : falaises en carrés autour des lacs, alignées sur les quads de l'atlas.
2. **Bruit de valeur** : `lib.rs:306-337` (smoothstep sur réseau cubique aligné aux axes, sans rotation d'octave) utilisé par `fractal_detail`. Sur une face du cube ce réseau est aligné avec (u,v). Les dérivées nulles aux nœuds donnent des facettes carrées à chaque octave.
3. Limiteur monotone du cubique (`sampler.rs:351-368`) : pentes mises à zéro aux extrema, donc palier par cellule d'atlas (visible vers 20–50 km d'altitude).
4. Dernières bandes sous-résolues (voir C2) : le maillage régulier de 128 segments voit du bruit blanc.

**Test discriminant** (à faire dans le navigateur) : comparer les modes `layer` 1 (altitude), 3 (humidité) et 0. Des carrés présents en mode 1 sont géométriques (C3/bruit). Des carrés seulement en couleur viennent de `biome_mix`. Des carrés seulement en éclairage viennent des normales de bord.

## 3. Pipeline LOD, requêtes, cache, mémoire

### P1 — Critique — Convergence sérialisée (≈ le 25 s et le 50 s)

**Où** : `crates/world-viewer/src/lib.rs`
- `2586-2698` (`refresh_cut`) : un seul split (`2677-2684`) ou une seule fusion (`2685-2690`) par cycle. Tout nouveau split attend que ses voisins aient fini le morph (`neighbors_settled`, `2679`).
- `2699-2944` (`process_cut`) :
  - tranche de 4 ms (`2872-2874`) ;
  - un seul job terminé par appel : `pending.cursor += 1; break;` (`2896-2897`) ;
  - les fusions attendent 0,35 s avant commit (`2704-2706`).
- `1008-1176` : un job « enfant » traite **tous** les sommets du mesh (`length = work.dynamic.len()`, `2756`), soit ≈ 50 000 : terrain, eau et shell.

**Mécanisme [INFÉRENCE]** :
- **Latence en chaîne par niveau** : `frontier_keys` (`965-1006`) ne demande que les enfants de feuilles déjà dans la coupe. Un `fly_to` à 30 km donne `wanted = 10` d'après `camera_level` (`1233-1235`) et `level_for` (`2100-2114`). Il faut donc ~10 niveaux successifs, chacun avec fetch + prépa + install + transition + 0,35 s de morph + attente des voisins.
- **Durée d'un split** : 4 jobs enfants × ⌈(50 k sommets × ~1–2 µs) / 4 ms⌉ ≈ 12–25 images chacun, soit environ 0,8–1,7 s, plus 0,35 s de morph.
- **Durée d'une fusion** : 1 job parent + 4 enfants, plus l'attente de 0,35 s, soit ~1–1,5 s.

Pour ~25 splits en série, cela donne 25–40 s. Pour ~50 fusions, 50–75 s. Ces ordres de grandeur sont cohérents avec le 25 s de convergence et le 50 s de dézoom observés. Le coût par sommet (1–2 µs) n'est **pas mesuré**.

**Solution minimale** (par ordre de gain/coût) :
1. **Budget adaptatif** : passer de 4 ms à 8–10 ms par image quand la caméra est immobile, et enchaîner plusieurs jobs par appel tant que le budget n'est pas consommé (au lieu d'un job).
2. **Lot par epoch** : planifier tous les parents prêts et non adjacents dans un même `PendingCut`/epoch. Le sérialisme voisin ne s'impose qu'entre transitions adjacentes.
3. **Chemin instantané** pour un split dont enfants et voisins sont hors champ, comme c'est déjà fait pour `coarsening` (`2492-2496`).
4. **Prefetch de lignée** : à un grand saut caméra (`fly_to`), demander l'ascendance de la tuile cible en une fois plutôt qu'un niveau à la fois.

**Mesure** : script `fly_to(30 km)` puis `window.worldBuilderPerf()` toutes les 500 ms. Journaliser `view.cut_commits`, `view.pending_tiles`, `view.max_prepare_ms`, `in_flight`, `queued`. Si `cut_commits` croît ~1/s alors que `in_flight == 0` et `queued == 0`, le goulot est le scheduler, pas le réseau. Critère de succès : convergence < 8 s.

### P2 — Haute — Éviction des ancêtres, dézoom lent, état `loaded` périmé

**Où** :
- `viewer lib.rs:2355-2356` : épinglés seulement `active_set` et le **parent immédiat** de chaque feuille.
- `lib.rs:2373-2379` : victime = `min((voulu?, last_used))`. Or `last_used` n'est mis à jour que dans `process_cut` (`2722`) et à l'installation, jamais pour les tuiles dessinées.
- `docs/IMPLEMENTATION.md:63` assume ce choix.
- `web/tile-stream.js:83-85` : `loaded` n'est recalculé qu'à l'**installation** d'une tuile, à partir des clés renvoyées par `install_prepared_tile`. Or des évictions ont aussi lieu au commit de coupe (`lib.rs:2714` et `2939`, `evict_cache()`).

**Conséquences** :
1. Après un plongeon, les ancêtres de niveau < L−1 sont les plus anciens, donc évincés en premier sous pression. Au dézoom il faut les refetch (worker puis serveur), puis passer un à un le scheduler de P1.
2. **Blocage possible [INFÉRENCE]** : une tuile évincée au commit reste « chargée » côté JS jusqu'à la prochaine installation. `dispatch()` (`tile-stream.js:44-58`) la saute (`this.loaded.has(key)`). Si le viewer attend ce parent pour fusionner et qu'aucune autre tuile n'est en cours d'installation, la fusion attend sans qu'aucune requête soit émise.

**Solution minimale** :
- **Épingler la lignée** de la feuille au centre de la caméra jusqu'à la racine (≤ 18 tuiles), et mettre à jour `last_used` pour toute tuile de la coupe active à chaque frame.
- **Résidence côté Rust** : exposer `resident_keys` dans `view_info()` (ou une fonction dédiée) et remplacer `loaded` par cette liste à chaque `updateDetail`.

**Test** : dézoomer de L=14 à L=5. Compter les requêtes réseau/worker pendant la fusion. Attendu après correction : ≈ 0 requête pour les ancêtres de la lignée, pas de plateau.

### P3 — Haute — Mémoire par tuile

**Où** :
- `viewer lib.rs:446-473` et `2006-2015` : `Tile.vertices: Vec<Sample>` est conservé en entier pour chaque tuile. Un `Sample` pèse 216 octets, plus une `String` de biome allouée à chaque sommet (`466`, `.to_string()`). Soit ≈ 3,7 Mio par tuile, 128 segments.
- `1078-1107` et `1171-1173` (`append_ocean_shell`) : le shell océan ajoute **tous** les sommets et **tous** les indices de la tuile, même sans océan. `compact_mesh` (`311-329`) ne retire pas ces sommets.
- `1909-1916` et `1917-1920` : `dynamic` (44 octets/sommet) et `geometry` (56 octets/sommet) conservés pour tous les sommets, shell compris.
- `2727-2729` : clones complets pour les jobs `Edge`.

**Estimation [INFÉRENCE]**, à vérifier avec `cpu_bytes / nombre de tuiles` dans `view_info()` :
- ≈ 9 Mio CPU et ≈ 6–7 Mio GPU par tuile.
- Le budget CPU de 768 Mio tient donc ~85 tuiles. Le champ visible peut atteindre 64 feuilles, plus parents et prefetch. Le viewer passe donc souvent en « pression » (`2641-2654`).
- La doc (`:65`) reconnaît que les « réserves de mémoire WASM » ne sont pas comptées.

**Solution minimale** :
1. Ne conserver par sommet que ce dont `stitch_target`, `displayed_radius` et les bordures ont besoin : direction, hauteur, niveau d'eau, id de corps, rivière. Soit `PackedSample` (120 octets) ou moins, sans `String`. Gain ≈ 1,5–2 Mio par tuile.
2. **Omettre le shell océan** quand aucun sommet de la tuile ni de sa couronne d'un rang n'a d'eau marine proche, et que le parent n'en a pas non plus. Gain ≈ 2,7 Mio GPU + 2,8 Mio CPU sur les tuiles intérieures.
3. Pas de `clone()` pour les jobs `Edge` : ne cloner que les sommets de bordure.

**Critère** : `cpu_bytes / tuiles` ≤ 4 Mio après correction.

### P4 — Haute — Les buffers GPU ne sont libérés que par le GC JS

**Preuve [DEP]** : `wgpu-27.0.1/src/backend/webgpu.rs:2774-2778` : `impl Drop for WebBuffer { fn drop(&mut self) { // no-op } }`. Même chose pour `WebTexture` (`2819-2823`).
**Preuve [CODE]** : aucun `.destroy()` dans `crates/world-viewer` ni `web/` (grep sur `destroy(`).

**Cause racine** : `evict_cache()` retire la tuile du `Vec` et recalcule `gpu_bytes` en conséquence, mais la mémoire GPU n'est libérée qu'à la collecte du `GPUBuffer` côté JS, non déterministe. Le compteur de budget sous-estime donc la mémoire réelle.

**Complément [INFÉRENCE]** : la mémoire linéaire WASM ne rétrécit jamais (propriété générale de `memory.grow`). La pointe de chaque installation (copie 6,5 Mio + 3 Vec de décodage + `Tile` + mesh + temporaires d'upload, ~25–30 Mio) laisse une réserve permanente. Le worker a sa propre instance WASM, avec le même défaut.

**Solution minimale** : implémenter `Drop for GpuMesh` qui appelle `self.vertices.destroy()`, `self.dynamic_buffer.destroy()` et `self.indices.destroy()` (API `wgpu::Buffer::destroy`, `buffer.rs:332`). Exposer la taille de la mémoire WASM (`wasm_bindgen::memory()`) dans `view_info()`.
**Mesure** : Chrome `performance.measureUserAgentSpecificMemory()`, plus le gestionnaire de tâches pour la mémoire GPU, avant et après un dézoom. À comparer avec `gpu_bytes`.

### P5 — Moyenne — Installation sur le thread principal

`install_prepared_tile` (`lib.rs:3335-3408`, JS `tile-stream.js:76-94`), une installation par image :
- copie du paquet (~6,5 Mio) dans la mémoire WASM ;
- `decode_prepared_tile` copie tout en `Vec` via `pod_read_unaligned` (`422-436`), plus un `String` par sommet (`466`) ;
- `rebuild_lookup()` (`3400`), `evict_cache()` (`3403`, qui appelle `requests()` en `2367`), puis `serde_json::to_string` de **toutes** les clés (`3405`), `JSON.parse` et `new Set(...)` côté JS ;
- la doc (`:63`) admet qu'une installation individuelle peut dépasser 4 ms.

**Solution minimale** : décoder sans copie (`bytemuck::try_cast_slice` sur un buffer aligné), un `u8` de biome au lieu d'une `String`, ne renvoyer que la clé ajoutée et les clés évincées, évincer en dehors du chemin d'installation.

### P6 — Moyenne — Jobs `Edge` coûteux

`lib.rs:2727-2729` clone `gpu.dynamic` et `gpu.geometry` en entier (≈ 3,3 Mio) pour ne modifier que ~1 000 sommets de bordure (`2735-2741`). `update_dynamic` (`1909-1916`) renvoie tout le buffer au GPU et recalcule `morph_pad` sur tous les sommets. Avec ~20 voisins par transition, cela fait ~60 Mio de copies et ~30 Mio d'upload par transition. [INFÉRENCE] Solution : écrire uniquement les plages de bordure (`queue.write_buffer` partiel).

### P7 — Moyenne — `covering_key` et voisinage

- `lod.rs:70-92` : 25 recherches de hash par sonde.
- `viewer lib.rs:942-962` (`transition_neighbors`) : pour chaque clé de la coupe, 8 sondes × 25 niveaux = ~200 recherches. Avec ~300 feuilles cela représente ~60 k recherches par plan, et le cache (`2559-2567`) est invalidé à chaque commit (`cut_serial`).

Solution : descendre depuis la racine d'ancêtre connue plutôt que balayer 25 niveaux, ou indexer les feuilles par (face, niveau, x>>k, y>>k). Gain probable de quelques ms par plan.

### P8 — Moyenne — Côté JS

- `tile-stream.js:29-33` et `63-64` : une tuile prête hors de `desired` est jetée, et un job qui sort de `desired` est annulé. Le calcul est perdu à chaque déplacement de caméra. Conserver les paquets prêts quelques secondes, et n'annuler que ceux dont le fetch n'a pas commencé.
- `tile-stream.js:49` : concurrence limitée à 3 fetchs et 6 en vol. Le serveur accepte 4 workers (`main.rs:251`). À mesurer avant de modifier.
- `web/tile-worker.js:35-41` et `71` : `openDisk` itère sur toutes les entrées de `CacheStorage` (jusqu'à 256, ~1,9 Mio chacune) avec un `match` par entrée, et chaque `rawTile` attend `diskReady`. Retard au premier chargement. À mesurer : sur `localhost`, le cache disque du worker double le cache serveur.
- `app.js:313` : `updateDetail` toutes les 50 ms appelle `view_info()`, qui recalcule `requests()` (`lib.rs:3601`) et `cache_bytes()` deux fois (`3603`). Négligeable (< 1 ms) mais inutile.

### P9 — Moyenne — Serveur

- `main.rs:571-600` : chaque écriture de tuile fait `read_dir` + `metadata` sur **tous** les fichiers du cache (jusqu'à 1 Gio ≈ 500 fichiers), sous un `Mutex` global, tout en tenant le permis du worker (`744-803`). Sous WSL2, surtout sur un disque monté `/mnt/*`, cela peut dominer le temps total sans apparaître dans `compute`.
- `main.rs:133-138` et `685` : `TileCache.get` prend un verrou **en écriture** à chaque requête, y compris les accès mémoire, et fait un `order.retain` O(n).
- `main.rs:779-786` et `preview.rs:53-60` : avec `phase`, chaque sommet rappelle `seasonal_sample`, qui rappelle `sample()`. Le coût de calcul des tuiles saisonnières double.

**Mesure** : comparer `compute;dur` et `total;dur` du `Server-Timing` sur des tuiles `generated`. Un écart > 50 % désigne l'écriture disque. `GET /api/performance` donne les cumuls.

## 4. Ce qui est cohérent (lu, sans défaut trouvé)

- **Sommets partagés** : deux tuiles de même niveau partagent des coordonnées entières exactes (`sampler.rs:228-237`), et les tests existants vérifient l'égalité de bits (`core lib.rs:1011-1051`, `sampler.rs:889-953`).
- **Partition de coupe** : `lod.rs` garantit un antichaîne et une couverture complète (tests `lod.rs:98-182`).
- **Racine et L0** : `preview.rs:41-65` produit les grilles de sommets L0 avec `world.tile`, identiques aux tuiles de niveau 0. La route `/tiles` par défaut (`coherent`) appelle aussi `w.tile` (`main.rs:774-775`). Le chemin `contracts` (`hierarchy.rs`) recalcule les hauteurs mais garde les drapeaux d'eau de l'atlas et déclare `coast_constrained: false`. Il n'est pas par défaut (`conservative-refinement` décoché).
- **Codec** : `tile_wire` valide magic, clé, taille de grille et indices.

## 5. Constats de basse priorité

| Réf. | Constat | Où |
|---|---|---|
| C6 | Hash de réseau XOR de trois produits ; collisions structurelles possibles. | `core lib.rs:325-328` |
| C7 | Les patches régionaux peuvent créer un océan inland (`wet` recalculé après modification de hauteur). Chemin non défaut (`generator=spatial`). | `core regions.rs:175-183` |
| C8 | Clés de normales par arrondi à 1e-12 ; une sommet de couture peut tomber de l'autre côté de l'arrondi et avoir une normale différente. | `viewer lib.rs:1276-1286`, `1513-1518`, `2410-2420` |
| P10 | `basin_owner` : balayage linéaire des lacs et de `lake.cells.contains`. Peu coûteux avec 16 lacs [DOC]. À indexer (cellule → lac) si la résolution monte. | `sampler.rs:416-451` |
| P11 | `set_world` fait passer ~100 Mio de JSON (JS → WASM) et conserve `world.cells` après `install_roots`. À libérer. | `viewer lib.rs:3296-3312`, `app.js:52` |

## 6. Plan d'action recommandé (ordre)

1. **C1** (enveloppe d'amplitude + règle `ocean`) avec **T1**. Gain direct sur lacs côtiers, îles et baies. Faible risque, 1 fichier.
2. **C3** (plafond lacustre continu) avec **T2**. Supprime les falaises en carrés.
3. **P1** (budget adaptatif, lots, chemin instantané) avec la mesure `fly_to`. Gros gain de convergence.
4. **P2** (lignée épinglée + résidence côté Rust) puis **P4** (`destroy()`). Corrigent le dézoom et la mémoire réelle.
5. **P3** (retenue réduite, shell conditionnel). Libère de la marge dans le budget.
6. **C4**, **C2** option B, **P5–P9**, après mesure.

## 7. Mesures à fournir à root (rien n'a été mesuré ici)

- **Navigateur** : `window.worldBuilderPerf()` sur un `fly_to(30 km)` puis zoom, et sur un dézoom de L=14 à L=5. Relever `cut_commits`, `pending_tiles`, `max_prepare_ms`, `max_install_ms`, `cpu_bytes`, `gpu_bytes`, nombre de tuiles, `in_flight`, `queued`, hits mémoire/disque/réseau du worker.
- **Serveur** : deltas de `/api/performance` (`compute_ms`, `queue_ms`, `evictions`) et `Server-Timing` par tuile, pour séparer calcul et écriture disque.
- **Monde** : `stats.lake_count`, `stats.lake_cells` et la distribution des tailles de lacs du monde de l'utilisateur, pour confirmer que les petits lacs ne viennent pas de l'atlas.
- **Tests natifs** : T1 (composantes côtières par niveau), T2 (continuité aux frontières de quad et de face), T3 (recensement des selles).

**Zones non couvertes** : érosion, rivières côté génération, UI, CUDA, `index.html`, rendu GPU réel. La partie GPU repose sur la lecture du code et du shader, jamais sur une capture ou une exécution.

## Réconciliation avec les mesures du monde utilisateur

Ajout par l’agent principal après l’audit, 3 octobre 2026 : le monde utilisateur est `83ee28acb8b3db81c39a`, graine 2386084681, atlas 512. Son manifeste donne **1 109 lacs physiques et 11 251 cellules lacustres**. L’hypothèse du résumé écartant l’hydrologie à partir de la graine 1337 ne s’applique donc pas à ce monde. Les mécanismes C1 et C3 restent présents dans le code initial ; les nombreux bassins physiques sont une deuxième cause. L’audit a été réalisé avant les corrections 0.7, et les lignes réfèrent à la révision inspectée.

Les corrections intégrées comprennent le déplacement horizontal de la côte, une amplitude verticale préservant le signe autour du niveau marin, la protection lacustre continue, le plafond LOD 24 et les normales des triangles minuscules, la destruction explicite des buffers GPU, le recalage de la résidence du scheduler et les corrections géologiques/hydrauliques. Les mesures finales et limites figurent dans IMPLEMENTATION.md ; le présent audit conserve les hypothèses et degrés de certitude de Sonnet.

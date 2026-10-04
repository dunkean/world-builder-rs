# Revue critique 0.10 (lecture seule, aucun test lancé)

Revue réalisée en CLI avec `claude-opus-5-5`. Le rapport ci-dessous conserve
l'état lu avant les correctifs ; il ne décrit pas leur validation finale.
La suite a ajouté une fermeture minimale 2:1 des subdivisions, vérifiée aussi
sur les coupes proposées pendant les transitions, et une régression du
soulèvement côtier. Les caches d'îles ont un front par thread ; les caches
lacustres séparent leurs quotas et ne gèlent que les objets touchant la requête.
Les mesures finales figurent dans les documents de validation 0.10.

En bref : le triangle vert vient très probablement de l'absence de contrainte 2:1 entre tuiles voisines. Le reste ne contient pas de bug prouvé, seulement des risques de performance.

## 1. Triangle vert côtier : géométrie

### Causes prouvées par le code

**A. Aucune règle ne limite l'écart de LOD entre deux tuiles voisines.**
- `lod.rs` : `split`/`coarsen` ne vérifient que les quatre enfants. `covering_key` accepte n'importe quel niveau entre 0 et 24.
- `refresh_cut` (`lib.rs:3016-3068`) :
  - une tuile est raffinée dès qu'elle est ancêtre d'une clé de `desired` et que ses enfants sont résidents ;
  - une tuile est regroupée dès qu'elle n'est ni ancêtre ni descendante de `desired`.
  - Le niveau des voisins n'est jamais consulté.
- `frontier_keys` (`1130`) applique la même logique.
- `desired` vient de `footprint_keys` (`1822-1884`) : des clés toutes au même niveau L, plus une bande de garde d'une tuile.
- Juste au-delà de cette bande, la feuille est le plus gros sous-arbre qui ne contient aucune cible. Elle peut donc être au niveau L−5 ou moins, par exemple de l'autre côté d'une limite de quadrant.
- Cette frontière devient visible dans trois cas :
  - vue inclinée près de l'horizon : les rayons qui manquent la sphère sont ignorés (`1850-1852`) ;
  - quand `footprint_level` réduit le niveau ;
  - pendant le zoom : le centre, préchargé en priorité, descend avant la périphérie.

**B. `stitch_target` (`2774-2791`) colle chaque sommet de bord fin sur la corde du voisin le plus grossier.**
- Il utilise `min_by_key(level)` puis `cached_surface(..., final=true)`.
- C'est nécessaire pour l'étanchéité. Mais avec un écart de k niveaux, un segment grossier couvre 2^k cellules fines.
- Dans `process_cut` (`3190-3199`) :
  - le sommet sol fin prend la hauteur de la corde grossière, par exemple +80 m entre deux caps ;
  - la première rangée intérieure garde sa vraie hauteur, par exemple −30 m dans la baie.
- On obtient un mur d'une seule cellule de large : un « coin » de terre levée. C'est exactement une forme de triangle 3D.

**C. Pourquoi il est vert et visible au-dessus de la mer.**
- Le sommet océan du même bord reçoit `final_depth = target.3`, la profondeur grossière, qui est ≤ 0 sur une corde terrestre.
- Le fragment est alors rejeté (`shader.wgsl:294`) : la mer disparaît sur cette bande et laisse voir le fond.
- `terrestrial_mix` (`lib.rs:588-626`) donne volontairement à tout échantillon de fond marin un biome climatique terrestre.
- `land_ink` (`shader.wgsl:227-244`) normalise par `total` : la moindre fraction interpolée produit une couleur végétale pleine.
- Résultat : fond marin levé, couleur verte. Il ne s'agit pas d'un halo.

### Hypothèse non prouvée

Pendant le zoom avant, les tuiles qui sortent de `desired` se regroupent en cascade, niveau par niveau, avec une animation visible quand `outside_view` est faux. Leurs sommets côtiers montent alors vers la corde du parent. Cela correspondrait au « triangle qui monte ». L'effet est amplifié par A, mais je ne l'ai pas observé.

### Inévitable ou évitable

- **Inévitable** : une tuile grossière dont la cellule enjambe une baie plus étroite qu'elle. Il en va de même pour la morph initiale d'un enfant depuis la corde parente (`nested_parent_source`, qui est correct : diagonale fixe a–d, cohérente avec `[a,b,d],[a,d,c]`).
- **Évitable** : un écart de plus d'un niveau entre une tuile fine proche de la caméra et sa voisine. Avec une contrainte 2:1, la hauteur du coin reste bornée par l'erreur d'un seul niveau.

### Régression proposée (fonction pure, sans GPU)

1. Prendre `footprint_keys(direction, 14, aspect, altitude basse, tilt≈1.2)`, avec une direction proche d'une limite de quadrant ou d'une arête de face.
2. Simuler la politique : partir de `initial_leaves`, découper toute feuille ancêtre d'une clé désirée et de niveau < 14, jusqu'à point fixe.
3. Vérifier que pour toute feuille `l` et tout `n ∈ tile_neighbors(l, cut)`, on a `|l.level − n.level| ≤ 1`.

Avec le code actuel, ce test doit échouer.

### Correctif minimal (une seule fonction)

Remplacer, dans `footprint()` (`2423`), les clés brutes par une version graduée :
- pour toute clé K qui produit des feuilles de niveau f, ajouter les 8 voisins de même niveau de `ancestor(K, f−2)` ;
- itérer du niveau le plus fin au plus grossier jusqu'au point fixe ;
- calculer les voisins avec `face_uv` + `key_at`, comme dans `footprint_keys`, pour gérer les arêtes de cube.

Point d'attention : `is_ancestor` est réflexif, et le filtre de raffinement découpe toute clé de `desired` de niveau inférieur à `wanted`. Une clé insérée au niveau m donne donc des feuilles au niveau m+1, d'où le « f−2 ».

Comme `refresh_cut`, `frontier_keys` et `evict_cache` passent tous par `desired`, le chargement, le raffinement et l'interdiction de regrouper suivent automatiquement. Le coût est de l'ordre de 16 clés supplémentaires par niveau.

## 2. Shader biome / contrat de coordonnées

**Vérifié correct :**
- `Vertex` fait 156 octets (39 × f32). Les attributs 0-4 et 9-11 sont dans le buffer statique, 5-8 dans le buffer dynamique.
- La matrice `noise_axes` est orthonormée, et identique côté CPU et WGSL.
- `cell + phase + q/m = noise_axes(R·anchor + delta)/m` : la phase géographique est conservée lors d'un rebasage ou d'un pan. Seul subsiste un arrondi f32 négligeable.
- La projection `surface_delta` sur la sphère est algébriquement exacte, sans perte de précision par annulation.
- Le domain warp dépend seulement du `raw` géographique de la bande parente. Une bande sautée implique que toutes les bandes plus fines le sont aussi, donc pas de discontinuité.
- `dpdx`/`dpdy` sont appelés avant tout branchement ou `discard`.
- Casts `i32` : R/2 m ≈ 3,2·10⁶ pour une Terre, dans les limites.
- Les drapeaux restent isolés : au plus 51 < 99. Le chemin eau n'appelle jamais `land_ink`, et les rubans sortent tôt.
- L'anti-aliasing est réel : la bande s'efface entre m/8 et m/2 par pixel.

**Risques (hypothèses) :**
- Les seuils `smoothstep(±0.2)` et la compétition `exp(bias)` resserrent les motifs fins. Un léger scintillement est possible avant que la bande ne s'efface.
- Le coût du bruit (144 `ink_hash` par fragment sur 6 bandes) n'est pas un problème sur une RTX 3090.
- Le vrai surcoût possible : le fond marin est entièrement ombré en terre sous la mer, et n'est rejeté que par le test de profondeur. Cela dépend de l'ordre de dessin, à mesurer.

## 3. `surface_detail.rs`

Aucun bug trouvé :
- `|relief| ≤ 1` est démontré : `ridge ∈ [−0.954, 0.338]`. La garde `amplitude.min(0.8·|h|)` (`sampler.rs:510`) conserve donc le signe par rapport à la mer, et la protection lacustre `0.8·depth` reste valide.
- `biome_mix` : ice + tundra + warm = 1 ; la somme est > 0 ; aucune fraction d'eau.
- Le gradient de température (lapse) est cohérent avec le sol reconstruit.

Détail sans conséquence : le nombre d'octaves dépend de `config.resolution` (arrêt quand R/fréquence < 0,5). C'est constant pour un monde donné, ce n'est pas un LOD.

## 4. Îles hiérarchiques (`landforms.rs`)

**Correct :**
- Couverture 3×3×3 : décalage radial ≤ 0,45·spacing plus portée d'île ≤ ~0,43·spacing, soit moins d'une cellule.
- Le saut `height >= max_height` est sûr, car target ≤ 1,25·peak.
- Le verrou est relâché avant `compile_bucket` : pas d'interblocage avec l'appel récursif à `height_until`.
- Le cache FIFO est déterministe.

**Risque de performance (hypothèse, à mesurer) :**
- `terrain()` prend 7 fois le **Mutex global** `IslandField::buckets` par échantillon, sous `par_iter`. Il est aussi appelé dans chaque callback d'environnement de `local_water` (environ 40 sondes par proposition).
- Sur les tuiles grossières, toutes les cellules océan sous ~6,7 m ratent le cache de 512 entrées des tiers fins (16 m / 62,5 m).
- Piste : un cache par thread (`thread_local`) ou découpé en plusieurs verrous indépendants.

## 5. `local_water.rs`

**Correct :**
- La densité n'est appliquée qu'une fois dans `suitability` : le pré-rejet et `freeze` utilisent le même tirage.
- Les halos `lo`/`hi` couvrent support + 0,55·spacing.
- Les identifiants sont distincts : bits 38-40 pour les micro-lacs, rivières à 1<<52.
- La protection 2 300 m est effective : chaque centre de lobe est testé, la distance de dégagement est 1-lipschitzienne, et la forme humide reste à moins de 2 300 m d'un lobe.
- Pas de nouveau tirage au pan ou au changement de LOD.
- `OnceLock` est sans réentrance : l'environnement n'appelle pas `local_water`.

**Mineur :**
- `CACHE_BUCKET_LIMIT` (1 024) est partagé entre les 4 tiers : les seaux de 192 m évincent ceux de 4 km. Le résultat reste identique, seul le recalcul coûte.

## 6. Divers

- `scripts/qa-renderer-browser.js:29` attend toujours `prototype-0.9.0`, alors que `GENERATOR_VERSION` vaut `prototype-0.10.0` : ce script de QA échouera.
- `spherical-coast-10` est cohérent entre `main.rs` et `app.js`. `WVP2` rejette bien `WVP1`.

## Prochain patch recommandé

Le « footprint gradué » 2:1 dans `footprint()`, avec le test de régression ci-dessus. C'est le seul défaut évitable qui explique le triangle. Les autres points sont à mesurer avant d'y toucher.

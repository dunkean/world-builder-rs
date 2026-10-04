# Revue du LOD instantané — Opus et Sonnet 5.5

Relectures indépendantes en CLI, en lecture seule, le 4 octobre 2026.
Modèles demandés : `claude-opus-5-5` et `claude-sonnet-5-5`. Les comptes
rendus ci-dessous portent sur le code lu avant les derniers garde-fous ;
les relecteurs n’ont exécuté aucun test.

## Analyse et décisions

Les deux lectures valident la couverture des vrais sommets, la graduation
2:1 et la revérification après préparation. Cette dernière est conservée.
Les deux risques signalés par Opus sont confirmés dans le code puis corrigés :
les feuilles sous la cible et sous l’œil incliné conservent leur animation,
et un emplacement d’epoch actif attend sa fin avant réutilisation.
Les tests reproduisent le nadir hors champ et 33 commits à 240 Hz.
Le rendu et le plan partagent le même repère, calculé une seule fois
par plan ; les bornes refusent un repère non fini. Un axe de secours
conserve un repère valide au pôle. Les racines conservées lors d’un
reset abandonnent explicitement la préparation annulée.

La réserve de Sonnet sur le diamètre projeté ne nécessite pas de changement :
pour un déplacement `(u,v,w)` autour d’un centre de profondeur `z`,
la différence de projection vaut exactement
`(u − (x/z)w, v − (y/z)w) / (z+w)`. La norme de cet opérateur est
`√(1+(x/z)²+(y/z)²)` ; le dénominateur est au moins `z−r`.
Le diamètre `2 f r √(1+(x/z)²+(y/z)²)/(z−r)` borne donc les deux
extrémités d’une sphère. Les tests comparent cette borne à des points
hors axe et proches du plan caméra.

Les recommandations facultatives de changement du bruit ne sont pas
appliquées à cette itération : le bruit de gradient est déjà validé en
browser, et ses coordonnées fixes sont conservées. Le rapport final de
rendu distingue cadence, convergence, état des caches et limites.


## Compte rendu Opus 5.5

# Revue critique : chemin de grossissement instantané et bruit de gradient

Lecture seule (Read/Grep), sans exécution de tests ni modification de fichiers. Les serveurs MCP context7, stripe et supabase demandent une autorisation et sont restés inutilisés. Ils sont sans rapport avec cette revue.

## Ce que j'ai vérifié et trouvé correct

- **`projected_diameter_pixels`** (`lib.rs:1967`) : la borne `2·f·r·√(1+a²+b²)/(z−r)` est correcte. Elle vient de la norme spectrale de la jacobienne de `(x/z, y/z)`, et `z−r` minore la profondeur de tout point de la sphère, donc les cas hors-axe et inclinés sont couverts. Si `depth ≤ r`, la fonction renvoie `INFINITY`, ce qui reste conservateur près du plan proche. La focale verticale vaut aussi en horizontal puisque `aspect = w/h`.
- **`sphere_in_frustum`** : demi-angle de 22,5° identique à la projection 45°. L'absence de plan proche et lointain rend le test plus large, donc conservateur. Le test « toutes les sphères hors champ » se fait sphère par sphère, ce qui est valide.
- **Union des bornes** : la boîte englobante, puis sa demi-diagonale, est conservatrice. Le décalage de morph est radial (`normal_height.xyz * mix(x,w)`, vecteur quasi unitaire), donc `morph_pad = max|morph[0]|,|morph[3]|` le borne bien.
- **Tuiles préparées sans GPU** : à la planification, le parent n'a pas de `morph_pad`, et sa couture vers un voisin plus grossier n'est pas couverte. Ce n'est pas un bug, parce que la revalidation (`lib.rs:3611`) a lieu après l'upload du `Parent` et l'appel à `update_edges`. Le pad réel est alors inclus, et l'ensemble de clés revalidé (parents, enfants, `Edge`) est le même que celui de la planification. **Toute la garantie repose sur cette revalidation : il ne faut pas la retirer.**
- **Retour en arrière quand le groupe devient visible** : les jobs `Coarsen` sont ajoutés, l'époque reste à 1e9 et `transition_start` est réécrit pour tous les jobs. C'est cohérent.
- **Gradation 2:1** : un `affected` plus fin que `L+1` est rejeté, et un lot d'un seul type (instantané ou non) est imposé. C'est cohérent avec les tests de voisinage exact.
- **Exagération** : `install_roots` vide le cache, donc les bornes sont recalculées depuis le mesh réel.
- **Mémoire** : le compte est correct pour `morph_offsets` (24 o par entrée), `prepared`, `dynamic`/`geometry` et `biome`. Une approximation acceptable : le mesh de `pending.work` sort de `prepared` et n'est compté nulle part pendant la préparation (transitoire).

## Bugs prouvés

Aucun dans le chemin instantané d'après cette lecture.

## Hypothèses à confirmer

1. **La caméra dépend de `active_set`.** `view_frame` et `render` calculent l'œil avec `ground_height` et `displayed_radius(eye_direction)` sur la partition **avant** le commit. Quand l'inclinaison dépasse 22,5°, le nadir de l'œil est hors champ et sa tuile peut être grossie instantanément. Si la contrainte `ground + 0.25·alt` est active (relief derrière la caméra), l'œil saute d'un coup, et c'est tout l'écran qui bouge, sans morph.
   - Correctif minimal : dans `can_coarsen_instantly`, refuser si un parent ou un enfant couvre `face_uv(eye.normalize())` ou `self.direction()`.
2. **Réutilisation des 32 emplacements d'époque.** Un commit instantané peut maintenant arriver à chaque image. À 240 Hz, 32 commits durent environ 0,13 s, moins que 0,16 s. Écraser l'emplacement (`start = 1e9`, id différent) donne `progress = 1` à un raffinement visible encore en cours, qui saute donc à sa forme finale.
   - Correctif minimal : dans `plan_transitions`, si `epochs[slot].start + MORPH_DURATION_SECONDS > now`, reporter (`cut_dirty = true; return false`).
3. **Précision f32 du recul de l'époque** (`start − 0,16` en f32) : `progress` peut valoir environ 0,9999 pendant une image. C'est négligeable et aucun correctif n'est nécessaire.

## Shader (`shader.wgsl`)

- **Gradients** : les 12 indices donnent bien (±x±y), (±x±z) et (±y±z), normalisés.
- **Interpolation** : les déplacements `f − coin` sont cohérents et l'interpolation quintique est C2. Le hachage des cellules négatives via `bitcast` est stable.
- **Phase** : la phase entière et fractionnaire, l'ancre scindée et le warp du parent brut sont cohérents avec `noise_frame`. Une bande fine visible implique que toutes les bandes plus grossières le sont aussi, donc la chaîne de warp est stable au zoom.
- **Hypothèse visuelle, pas un bug de raccord** : les échelles 64 km → 2 m forment des réseaux emboîtés (rapport 8) sans décalage propre à chaque bande. Le bruit de gradient s'annule à chaque coin, donc aux coins de 64 km toutes les bandes valent 0 et le warp aussi. On obtient un motif neutre, ponctuel et régulier.
   - Correctif optionnel : ajouter un décalage fractionnaire fixe par bande dans `noise_frame`.
- **Contraste** : la distribution du bruit de gradient est plus étroite que celle du bruit de valeur. Le gain de 1,8 et les seuils des `smoothstep` sont à réévaluer visuellement, mais ce n'est pas une erreur.

## Recommandations

N'appliquer que les correctifs des hypothèses 1 et 2, chacun avec un test de régression : nadir hors champ en incliné, et cycle de plus de 32 époques en moins de 0,16 s. Garder la revalidation au commit telle quelle.

Je n'ai lu ni les revues Opus et Sonnet existantes ni les tests de voisinage. Ces conclusions sont indépendantes.


## Compte rendu Sonnet 5.5

# Revue critique : coarsening instantané, `world-viewer/src/lib.rs`

Revue par lecture seule, sans exécution de tests ni de code. Les tests existants (voisins locaux contre scan complet, toutes faces et coins) n'ont pas été relancés. Je ne les ai donc ni confirmés ni infirmés.

## Bugs prouvés

Je n'ai trouvé aucun bug prouvé dans le chemin rapide. Les éléments ci-dessous sont des vérifications faites, puis des hypothèses.

## Vérifié correct

- **Frustum :** `view_frame` (`lib.rs:2833`) reproduit exactement l'œil, le `forward` et le `right` de `render` (`:3681-3693`). Le test de frustum est le même que celui du culling de draw (`:3703`), avec le même `aspect()` et le même FOV vertical de 45°. Un groupe jugé « dehors » n'est donc dessiné par aucun tile.
- **Sphères englobantes :** `mesh_bounds` est un AABB dont le rayon est la demi-diagonale, donc une sphère englobante valide. Le pad de morph ajoute `max(|morph[0]|, |morph[3]|)`, ce qui couvre `mix(x, w, progress)` dans le shader (`shader.wgsl:52`). L'union des sphères en AABB est aussi englobante.
- **Exaggeration :** `set_exaggeration` appelle `install_roots`, qui annule le pending et vide les tiles. `install_prepared` rejette les préparations d'une autre exaggeration. Les bounds ne peuvent donc pas être périmés.
- **Tiles préparés sans GPU :** `CachedTile.bounds` vient du mesh préparé. Le pad vaut 0 sans GPU, ce qui est trop optimiste au plan. La revalidation finale (`:3611`) a lieu après l'upload des jobs Parent et Edge, avec le pad réel. Elle est dans la même fonction que le commit, sans trou temporel.
- **Revalidation :** l'échec rebascule sur le morph de 0,16 s en ajoutant des jobs `Coarsen`, avec une base `current_*` recalculée. L'époque reste à 1e9 jusqu'au démarrage, donc rien n'est visible entre-temps.
- **Commit instantané :** `epoch_start = start − MORPH_DURATION` donne `progress ≈ 1`, et `transition_start` passé libère `neighbors_settled`.
- **Gradation 2:1 :** `affected.any(level > parent.level + 1)`, la complétude des enfants dans `proposed` et la disjonction des footprints sérialisent les fusions adjacentes. Le mélange instantané/morph dans un lot est refusé (`:3120`). Je ne vois pas de chemin vers un écart de LOD supérieur à 1.
- **Mémoire :** `cpu_bytes` compte la capacité des `Vec` de samples, des chaînes de biome, des indices, des bordures, des offsets de morph, du mesh préparé, ainsi que `dynamic` et `geometry`.

## Hypothèses, ordre de gravité décroissant

1. **Borne de diamètre, au second ordre.** `projected_diameter_pixels` utilise `L` au centre. La borne exacte est `L_q ≤ (|p_xy| + r)/(depth − r)`.
   - L'écart est de l'ordre de `r/d`, soit environ 1e-3 relatif à 0,5 px. Il est pratiquement sans effet, mais la borne n'est pas strictement prouvée.
   - Correctif minimal : remplacer `lateral` par `((hypot(px, py) + radius)/(depth − radius))²`.
2. **Coût du plan.** `can_coarsen_instantly` rappelle `view_frame` à chaque candidat. Chaque appel fait `ground_height` plus deux `displayed_radius`, donc trois `cached_surface`. Le plan est budgété à 2 ms. Corriger en calculant le frame une fois par `plan_transitions` et en le passant en paramètre.
3. **Frame non fini.** Regarder exactement le long de l'axe Y donne `normalize()` NaN. `sphere_in_frustum` renvoie alors `false`, soit « dehors », et `instant_coarsen_bounds` renvoie `true`. Rien n'est dessiné dans ce cas, donc le dégât visuel est nul. Ajouter quand même un garde `frame` fini : c'est une ligne et ça évite un faux positif.
4. **Marge de 1e-6 rayon planétaire (~1,7 m).** Elle gonfle les sphères et rend l'instantané rare près du sol en zoom profond. C'est conservateur, donc sans risque, mais ça réduit le bénéfice là où il compte.
5. **Dérive du code dupliqué.** `render` recalcule le frame au lieu d'appeler `view_frame`. Le jour où l'un change, la garantie « frustum exact » disparaît en silence. Faire appeler `view_frame` par `render`.
6. **`morph_pad` monotone.** `update_edges` ne le diminue jamais. C'est conservateur, mais le pad grossit avec l'historique et pénalise culling et fast path.
7. **Comptabilité transitoire.** Le `CutWork` en vol (mesh, dynamic, geometry) n'est pas compté pendant `prepared.take()`. Le dépassement est borné à un tile.
8. **Éviction.** `swap_remove` suivi de `rebuild_lookup` est en O(n²) sous pression mémoire. La sortie sans victime laisse silencieusement la limite dépassée.
9. **Précision temporelle.** `epoch_start` passe par f32. Après environ 1e5 s de session, `progress` peut valoir 0,998 au lieu de 1 : invisible.

## Shader (`shader.wgsl`)

Je n'ai trouvé aucune erreur de couture, de phase ou de dérivée.

- **Gradients :** les 12 gradients correspondent à la table classique. `select(faux, vrai, cond)` est utilisé dans le bon sens, et le facteur 0,7071 les normalise.
- **Interpolation :** la quintique est correcte, et les déplacements `f − corner` aussi.
- **Phase :** `noise_axes` est orthonormée (j'ai vérifié les produits scalaires). `noise_frame` factorise bien phase fractionnaire + cellule entière ; le hash porte sur la cellule absolue, indépendante de l'ancre.
- **Reprojection :** `surface_delta` est algébriquement exacte, car `(1−norm)(1+norm) = −t`.
- **Dérivées :** `dpdx` est calculé avant les branches et `discard`.
- **Bandes :** le warp utilise `raw` non filtré. Les bandes fines sautées sont les dernières, donc la chaîne de warp ne dépend pas du zoom.
- **Risque faible :** `footprint_m` vient d'un quad 2×2 et peut sauter à la frontière de deux LOD. Le fondu `smoothstep` l'adoucit.

## Recommandation minimale

Faites 1 (borne exacte), 2 (frame hoisté), 3 (garde NaN) et 5 (un seul `view_frame`). Laissez le reste. Un test de revalidation avec un GPU simulé serait utile, mais il demande un harnais que le dépôt n'a pas.

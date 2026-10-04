# Revue croisée 0.10 (lecture seule, rien exécuté, rien testé)

Revue réalisée en CLI avec `claude-sonnet-5-5`, en complément d'Opus.
Le rapport conserve les constats à la lecture. Les suites ont corrigé le test
d'espacement, ajouté la régression terrain/eau 64→32, conservé 64 segments pour
`contracts`, et borné la coupe complète à **1 024 feuilles**. L'insertion des
ancêtres s'arrête au premier déjà présent ; le premier candidat de regroupement
réutilise le cache des voisins. Les 54 tests du viewer passent après ces
correctifs. La pente dépend toujours des normales du maillage résolu : cette
limite reste valable. La validation graphique est documentée séparément.

L'arbre a changé pendant ma lecture. Le passage à 32 segments (`DEEP_STREAM_SEGMENTS`), les plafonds à 1024/1536 et `app.js` (`tile.level <= 2 ? 64 : 32`) sont apparus entre deux lectures. Ce qui suit vaut pour l'état lu en dernier. La revue Opus est en partie périmée.

## 1. Défauts prouvés par le code

1. **Test périmé qui échouera.** `streamed_grid_keeps_physical_vertex_spacing_after_one_extra_level` (`lib.rs:~5026`) suppose `camera_level == previous+1` et `STREAM_SEGMENTS·2^L == 128·2^prev`.
   - `camera_level` (`lib.rs:1606`) ajoute désormais +1 pour `level > 2`.
   - À l'altitude 3e-6, le niveau est 22 pour un `previous_level` de 20. L'assertion `level == previous+1` échoue.
   - Pour `level > 2`, la bonne égalité est `DEEP_STREAM_SEGMENTS·2^L == 128·2^prev` avec `L == prev+2`.
   - Je l'ai déduit par lecture du code.
2. **Aucun test pour la transition 64→32 (L2→L3).**
   - Les sommets de l'enfant à 32 segments tombent exactement sur les sommets du parent à 64. L'espacement est identique et la diagonale a–d est la même.
   - Le repli `parent_source` (`vertices.len() != side²` → `cached_surface`) la gère. `ray_triangle_detail` accepte un sommet avec une tolérance de 1e-7.
   - Le test existant ne couvre que 128→64.
3. **Seul le générateur `contracts` dépend des segments.**
   - `hierarchy.rs:141-148` (`family_seed`) et `families.rs:49` hachent `segments`. Le mode conservateur (`conservative`) donnerait donc un champ différent à 32 et à 64.
   - Le chemin `coherent` est pur. `Sampler::tile` appelle `sample(direction)` par sommet, donc le champ ne dépend pas de la grille.

## 2. Verdict sur le LOD

**L'écart 2:1 est correctement implémenté, et mieux que ce que proposait Opus.**
- `graduate_targets` clôt les parents à scinder. Pour chaque parent de niveau p, il ajoute les ancêtres de niveau < p de ses 8 voisins de même niveau. L'ensemble reste clos par ancêtres.
- Pour un L donné, la scission de P exige que ses voisins existent au niveau p, donc que leurs ancêtres soient scindés. La logique est correcte.
- **Arêtes et coins de cube :** les sondes de `same_level_neighbors` sont décalées d'une tuile entière, avec une distorsion gnomonique inférieure à 0,5 cellule. Elles restent donc dans la cellule alignée. Je ne vois pas de bug.
- **Le test a un oracle indépendant.** `assert_graded_cut` s'appuie sur `tile_neighbors`, qui sonde à ±1e-6 depuis l'arête. Il ne partage pas le chemin de `same_level_neighbors`.
- **Les autres garde-fous tiennent :**
  - `split_preserves_grading` exige `neighbor.level >= key.level`.
  - `coarsen_preserves_grading` exige `<= parent.level+1`.
  - `needs_refinement` est cohérent, car `targets` est clos par ancêtres et `is_ancestor` est réflexif.
  - `graduated_leaf_count = 6 + 3·splits` est exact.

**Pas de deadlock logique sous pression mémoire.**
- Sous pression, `pressure_frontier` ne garde que le quartet de `staged_parent`, qui est le `needs_refinement` de plus bas niveau.
- Par la clôture, un voisin plus grossier d'un parent donné est lui-même `needs_refinement` et de niveau inférieur. Le staged ne peut donc pas être bloqué par `split_preserves_grading`.
- `evict_cache` épingle ce quartet.
- Le test `hot_center_waits_for_cold_support…` couvre le cas « seul le centre est chaud », mais sans pression.
- Risque résiduel : si les feuilles actives plus leurs parents épinglés dépassent 768 MiB, l'éviction s'arrête (`break`). Le mode pression devient permanent : un quartet à la fois et plus de prefetch. C'est lent, pas bloqué.

**Le triangle vert.** La cause décrite par Opus (mur d'une cellule via `stitch_target` + sommet océan à profondeur ≤ 0 ⇒ `discard` ⇒ fond marin coloré comme de la terre) est cohérente avec le code lu (`stitch_target`, `shader.wgsl:310`). Je n'ai pas relu `terrestrial_mix`, donc je ne la confirme pas. Avec 2:1, le mur reste borné à une erreur d'un niveau. Il n'est pas supprimé.

## 3. 32 segments : est-ce mieux que de baisser la résolution ?

Oui pour le champ proche. Mais le gain n'est pas « ÷4 au total ».
- `footprint_keys` lance seulement 13×13 rayons plus une garde 3×3. Le nombre de tuiles dépend donc des sondes, pas de la surface.
- Les feuilles (anneau ≈ constant par niveau) restent donc à peu près le même nombre. Chacune coûte 1089 sommets au lieu de 4225.
- Les plafonds sont passés ×4 (384→1536). Le budget total reste équivalent : 1536×1089 ≈ 1,67 M de sommets, soit environ 384×4225. C'est ce qui permet au 2:1 littéral de 660–876 feuilles de tenir (≈ 0,95 M de sommets) sans abaisser le LOD voulu.
- **L'espacement au centre est identique.** `camera_level` +1 le garantit, et le `step` utilise bien `DEEP_STREAM_SEGMENTS`.
- **Coûts :**
  - L'espacement à distance moyenne double à structure d'anneau égale (une tuile par niveau, donc ≈ d/seg).
  - Il y a plus de tuiles : requêtes, draw calls, planification.
- **Autres points vérifiés :**
  - L'en-tête WVP2 accepte 33² (le plafond de validation est 263169).
  - La clé du cache disque contient `segments`.
  - Aucun 64/65 codé en dur dans `web/` hormis `app.js`.
  - `main.rs` annonce déjà `tile_segments:32` et `coarse_tile_segments:64`.
  - `api_default_tile_segments` reste à 64 côté serveur : bon si les clients externes comptent dessus.
- **À mesurer avant de valider :** les sorties imprimées du test « real shore L19–22 » (`raw`, `leaves`) doivent donner `raw ≤ 1024` et `leaves ≤ 1536`.

## 4. Shader

**Prouvé par lecture :**
- **Phase stable au zoom et au pan.**
  - `surface_delta` est exact sur la sphère. L'ancre est en deux flottants (haut/bas) et l'ancre f64 donne la cellule entière, donc le rebasage est neutre.
  - Le fondu par `footprint_m` ne module que l'amplitude.
  - Le warp utilise le bruit brut non filtré, donc rien ne glisse.
  - `dpdx`/`dpdy` sont avant tout `discard`.
- **La formule de pente est exacte.** `tan_phys = tan_rendu · (R+e·h)/(R+h) / e`. `scene.surface.x` reçoit bien l'exagération (`lib.rs:3575`).

**Avertissements (hypothèses, non observées) :**
- **La classification dépend du maillage.** La pente vient de `shading_normal`, calculée sur le maillage. Une tuile plus grossière lisse les pentes, donc des arbres sur falaise, puis de la roche quand le LOD fin arrive.
  - Le morph des normales dure 0,16 s.
  - `biome_a/b` ne sont pas morphés.
  - Les normales ne sont partagées qu'entre tuiles de même niveau (clé `(level, normal_key)`), donc un joint de teinte est possible à la frontière 2:1.
  - Les seuils raides (roche 40→65°) amplifient l'effet.
- **La neige n'existe que dans le biome 0 (glace).** Il n'y a pas de neige de pente ni d'altitude ailleurs. Dans la glace, la pente compte deux fois : `exposed` dans `debris`, puis `snow_cover`. C'est cosmétique.
- **Les fractions de biome ne sont pas conservées en surface.** `pow(f,1.35)·exp(biais)` avec un biais borné à ±6 permet à une fraction de quelques pourcents de gagner localement.
- Les sombres sous-bois et clairières (`tree_cover`, `cover` ≥ 0,60) sont cohérents avec votre intention. Le contrôle navigateur réel reste à faire.

## 5. Îles et lacs : points d'Opus devenus obsolètes

- **Mutex des îles.** Un cache `thread_local` (`ISLAND_FRONT`) précède désormais le `Mutex` global (`landforms.rs:303-338`). Le verrou est relâché avant `compile_bucket`. Il reste un double verrouillage sur un raté (hypothèse perf).
- **Quotas des lacs.** Les quotas sont déjà par tier (`BUCKET_TIER_LIMITS`, `PROPOSAL_TIER_LIMITS`). L'éviction croisée entre tiers décrite par Opus n'existe plus.
- **Pas de verrou tenu pendant les rappels.** `bucket_slots` prend un seul verrou pour les 4 tiers. Le verrou du cache n'est pris qu'après `freeze`, pour les statistiques, jamais pendant `environment(p)`. L'ordre `OnceLock` → mutex des îles ne forme pas de cycle.
- **QA.** `qa-renderer-browser.js:29` attend déjà `prototype-0.10.0`. L'alerte d'Opus est périmée.

## 6. Hypothèses de performance (à mesurer)

- **`graduate_targets`.** Pour chaque parent, 8 voisins × p ancêtres sont insérés dans un `HashSet`, soit environ 10⁵–10⁶ opérations. Elle est appelée plusieurs fois par image : `bounded_footprint_level` en boucle, `footprint()`, `requests_at`.
- **`coarsen_preserves_grading`.** Elle appelle `transition_neighbors` sans cache, en O(|cut|·8 sondes), avec un cut qui peut monter à 1536. Un seul candidat peut dépasser le budget de 2 ms de `plan_transitions`. La version appelée via `affected_neighbors` est, elle, en cache.

## Recommandation minimale unique

1. Corriger le test périmé du point 1. Les valeurs attendues sont `level == prev+2` et `32·2^L == 128·2^prev` pour `level > 2`.
2. Ajouter dans la même passe un test 64→32 qui reprend `first_64_segment_children_repartition…` avec `side=65` et un enfant à 33.

Le profilage de `graduate_targets` peut attendre une mesure réelle. Si elle confirme un coût, l'optimisation est un changement d'une ligne. L'ensemble étant clos par ancêtres, il suffit d'itérer `(0..p).rev()` et de s'arrêter au premier `insert` qui renvoie `false`.

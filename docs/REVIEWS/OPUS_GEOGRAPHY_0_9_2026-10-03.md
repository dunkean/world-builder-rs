# Revue Opus 5.5 — géographie 0.9

Revue CLI en lecture seule (`claude-opus-5-5`), le 3 octobre 2026. Le texte ci-dessous porte sur une révision intermédiaire ; il ne constitue pas une validation du code final. Certains fichiers ont évolué pendant la lecture.

Corrections intégrées après la revue :

- A1 : les îles précèdent le creusement des fjords, qui raccorde le terrain relevé ; régression de chevauchement.
- A2 : index sphérique des domaines lacustres et seuils ; poids continu pour les nouveaux reliefs, exclusion des supports entiers pour îles, troncs, branches et microbassins.
- A3 : une frange locale sèche ne devient pas un lac après incision fluviale.
- A4 : moyenne pondérée par supports et crêtes évaluées par paires actives.
- A5 : centre réel, sondes des lobes et du contour extérieur ; tolérance adaptée à la taille.
- A6 : hauteur des îlots bornée par leur petit axe et mise à l’échelle du rayon.
- A7 : climat annuel pour les caches, invalidation explicite et garde de support en release.
- B1 : les microbassins partagent le domaine atlas des autres formes ; leur halo ne traverse plus deux métriques de transport.

Les budgets physiques des nouveaux objets restent une limite explicite du prototype. Les mesures finales sont dans `GEOGRAPHY_0_9.md`, `QUALITY_REFERENCE.md` et `RENDERING_0_9.md`.

---

# Revue critique : provinces géographiques et petits lacs glaciaires

J'ai seulement lu le code, sans rien modifier ni exécuter. Rien de ce qui suit n'a été testé : les « défauts prouvés » le sont par lecture du code et par calcul, pas par exécution.

Fichiers lus : `landforms.rs`, `local_water.rs`, `sampler.rs`, `coast.rs`, les extraits concernés de `lib.rs`, `river_geometry.rs` (destinations et `compile`) et l'appel `/landforms` dans `main.rs`.

---

## A. Défauts prouvés

### A1. Une île qui chevauche un fjord efface le fjord et crée une falaise (gravité haute, très probable en pratique)

- **Où :** `landforms::terrain`, `landforms.rs:464-491`.
- **Mécanisme :**
  - Dans la boucle, les fjords passent d'abord et abaissent `height` sous `natural`.
  - Ensuite, chaque île fait `height.max(natural + (target - natural) * support)`.
  - Quand `support` tend vers 0 (`radial` vers 1.6), cette valeur tend vers `natural`, qui est au-dessus du fond du fjord. Le `max` restaure donc le terrain naturel dans tout le disque de l'île.
  - Juste à l'extérieur (`radial ≥ 1.6`), le fjord creusé reste. On obtient une marche de `natural − fond_fjord`, soit jusqu'à des centaines de mètres, sous l'eau ou à terre.
- **Pourquoi c'est probable :** les deux boucles de `compile` (`:162` et `:291`) parcourent la même liste `coast`, triée par la même clé aléatoire.
  - Tout site de fjord est aussi candidat archipel au même rang, sauf s'il est à moins de 160 km d'un archipel déjà retenu.
  - Le cadre de l'archipel est à 22 km au large de la même cellule, avec des îles à ±13 km en travers et jusqu'à environ 14 km de support. Il recouvre donc souvent les 6 km de tronc côté mer.
- **Correctif minimal :** calculer les îles par rapport à la hauteur déjà creusée, pas par rapport à `natural`. L'ordre des districts (fjords, puis archipels, puis boucliers) est déjà garanti par l'ordre de `push` et les listes de bucket croissantes.
  - Exemple : capturer `let base = height;` à la première entrée dans un bras `Archipelago`.
  - Puis `height = height.max(base + (target - base) * support)`.
  - Optionnel : rejeter à la compilation une île dont le support croise un tronc, pour garder la connexion fjord–mer.

### A2. Les reliefs procéduraux (fjords, îles, cuvettes) sont coupés net à la frontière des réservoirs physiques

- **Où :**
  - `sampler.rs:73-77` et `geographic_height` `:568-572` : `landforms::terrain` est sauté si `lake_datum` existe.
  - `local_water_at` `:636` : le lac local est supprimé si `lake_datum` existe, et aussi si `natural < 0`.
- **Mécanisme :** `lake_datum` est binaire (`radius < 1`). Une tranchée de fjord (jusqu'à 31 km vers l'intérieur) ou une île (placée là où `atlas_height ∈ [-2400, -5]`, donc possiblement dans un réservoir sous-marin fermé) qui traverse le contour `radius = 1` donne une discontinuité C0.
  - Pour les cuvettes, `freeze` (`local_water.rs:309-358`) ne teste la protection qu'au centre d'adresse et sur 12 points du contour φ=0.
  - L'anneau φ∈(0, 0.65) et les lobes non sondés peuvent entrer dans le support d'un réservoir. Le lac disparaît alors au milieu de son support, avec une marche allant jusqu'à `w·(profile − natural)`, soit plusieurs mètres.
- **Effet aggravant :** un tronc de fjord peut traverser le seuil de déversement gelé d'un lac. Ce seuil est hors support, donc non protégé : l'invariant de `fine_detail_cannot_erase_a_frozen_dry_lake_spill_control` n'est plus garanti dès que des provinces existent. Ce test les désactive implicitement : `synthetic` utilise `coast_detail_ratio` à 0, mais `fjord_strength` garde sa valeur par défaut.
- **Correctif minimal (à la compilation, pas par point) :**
  - Dans `Landforms::compile`, rejeter un tronc, une branche ou une île si l'un de ses points échantillons (extrémités, milieux, ±largeur) tombe dans `lake_datum`, sur une cellule `lake`, ou près d'un `control_level` ou exutoire.
  - `reconstruction` et `lake_geometry::supports` n'appellent pas `landforms`, donc il n'y a pas de récursion.
  - Pour `freeze` : sonder aussi le contour extérieur φ = SUPPORT_PHI (12 à 16 rayons). Ensuite, la coupure par point dans `local_water_at` devient sans effet visible.

### A3. Une donnée de micro-lac « sèche » inonde les chenaux de rivière creusés

- **Où :** `World::sample`, `sampler.rs:84` (`water.lake || water.height_m >= water.level_m`), puis `:158-163`.
- **Mécanisme :**
  - Sur la frange sèche (hauteur ≥ niveau), la donnée du micro-lac est conservée comme métadonnée.
  - Le creusement des rivières abaisse ensuite `height`. Si `height < level`, alors `lake = true` et la rivière est remplacée par le plan du micro-lac.
  - En amont, cela produit une marche de surface égale à la profondeur de la rivière. En aval, le plan du lac s'étend dans le lit sur tout le support.
- **Pourquoi c'est atteignable :** la protection de `local_environment` (`:595-617`) ne couvre que les cellules à `discharge ≥ 120`.
  - Elle ignore les tronçons d'exutoire et de contrôle de déversement à faible débit, qui sont pourtant des tronçons dans `river_geometry::compile` (`:344-351`).
  - Elle mesure la distance à la corde droite de l'atlas, alors que les tronçons réels sont courbes.
- **Correctif minimal :** pour une donnée locale, utiliser `local_water.lake` (décidé avant creusement) au lieu de `height < level`. Une donnée locale sèche ne doit jamais devenir `lake` après le passage des chenaux.

### A4. Petite discontinuité de hauteur quand un support voisin entre dans la moyenne

- **Où :** `local_water::sample`, `:513-520`.
- **Mécanisme :** une vue qui entre (φ_v → 0.65⁻) reçoit `influence = 1 − smooth(gap/0.18)`. Ce poids n'est pas nul si le propriétaire est lui-même dans l'anneau (φ_o > 0.47). Sa hauteur vaut `natural`, puisque w_v = 0.
  - La moyenne saute de `h_o` à `(h_o + k·natural)/(1+k)`.
  - Ordre de grandeur : environ 0.02·|profile − natural|, donc quelques cm à environ 0.2 m.
  - Le terme `ridge`, lui, est bien continu.
- **Correctif :** pondérer tout le monde, propriétaire compris, par `k_i = influence_i · w_i` (w = 1 − smooth(φ/0.65)). Faire la moyenne sur les écarts : `height = natural + Σ k_i (h_i − natural) / Σ k_i`.
  - Cela reste symétrique au changement de propriétaire, continu à l'entrée d'un support, et inchangé à l'intérieur des lacs (w = 1).

### A5. Les rives sont sondées depuis un seul lobe ; le centre testé est le mauvais point

- **Où :** `Basin::freeze`, `:309` et `:334-358`.
- **Problème 1, les lobes lointains :** les 12 rayons partent du lobe central et trouvent le premier passage φ=0. Pour une forme en banane de 4 à 6 lobes avec bras latéral, les lobes d'extrémité et le bras ne sont pas sondés.
  - Le fond est synthétique (w = 1 pour φ ≤ 0, indépendant de `natural`).
  - Résultat : soit une fosse taillée dans une pente, soit un lac « perché » retenu par la levée synthétique φ=0, à `level + 0.18` au-dessus d'un terrain plus bas.
- **Problème 2, le centre :** l'aptitude (province, climat, rugosité, protection) et `center.natural_height_m` sont évalués au centre d'adresse. Le commentaire `:332` admet que ce point peut être sur un golfe sec.
- **Correctif :**
  - Évaluer `center` en `direction_at(anchor.x, anchor.y)`.
  - Sonder 2 à 4 rayons par lobe, y compris le bras.
- **Tolérance trop large pour les petits lacs :** elle vaut `8 + 0.014·L` (`:361`). Pour un étang de 50 m, l'anneau φ∈[0, 0.65] fait quelques mètres de large et peut monter jusqu'à 8 m, soit une pente d'environ 70 à 80°.
  - Correctif : une tolérance proportionnelle à la taille, par exemple `0.5 + 0.02·length_m`.

### A6. Îlots en aiguille

- **Où :** `landforms.rs:328-342`.
- **Mécanisme :** pour les îlots mineurs, `peak` (15 à 195 m) ne dépend pas de `long` (90 à 2290 m) ni de `short` (0.25 à 0.65 × long).
  - La pente au rivage selon le petit axe vaut à peu près `2·peak/short`.
  - Exemple : 100 m par 30 m pour 150 m de haut donne une pente d'environ 10, quasi verticale.
  - Environ 20 % des îlots mineurs dépassent une pente de 2.
- **Correctif :** `peak = peak.min(0.5 * short_m)`, et mettre aussi `peak` à l'échelle `scale` pour les petites planètes.

### A7. Saisonnier et clones (latent dans l'application actuelle)

- **Climat saisonnier dans la compilation :** `Landforms::compile` (`:144-147`) lit `c.temperature_c` et `c.moisture` bruts. `local_environment`, lui, passe correctement par `annual_cell_climate`, dont j'ai vérifié qu'il reproduit exactement `ClimateField`.
  - Une vue issue de `seasonal_world` qui compile ses propres provinces obtiendrait d'autres boucliers, donc d'autres lacs. Le `seasonal_sample` d'un monde saisonnier clone en plus ces caches.
  - Le serveur n'échantillonne aujourd'hui que des mondes annuels, d'où « latent ».
  - Correctif : `annual_cell_climate(world, c)` dans `compile`.
- **Commentaire faux sur le clone :** `impl Clone for LocalWaterAtlas` (`local_water.rs:59-63`) affirme que le clone part vide. C'est faux : `World::clone` clone l'`OnceLock<Arc<…>>`, donc l'`Arc` est partagé et ce `Clone` n'est jamais appelé.
  - Un clone qui modifie ses cellules sans `invalidate_geometry` réutilise les bassins.
  - Correctif : corriger le commentaire, ou faire un `take()` des caches à la main dans les chemins de clonage diagnostique.
- **Garde de support seulement en debug :** `debug_assert!(support_m < MAX_SUPPORT_M)` (`:239`). Analytiquement, le pire cas théorique atteint environ 2309 m, au-delà de 2300 ; c'est improbable mais pas impossible. En release, le halo du cache pourrait alors couper un contour selon le bucket.
  - Correctif : `if support_m >= MAX_SUPPORT_M { return None; }`.

---

## B. Hypothèses fortes (non démontrées)

### B1. La marge de garde province est probablement trop faible face au transport côtier (q contre p)

- **Le décalage de domaine :** les bassins sont adressés et mesurés en **p** (affichage). `near_lake_province` (`sampler.rs:637`) teste en **q** avec une marge de seulement 200 m (`radius·1.05 + 2500` contre 2300 de support).
- **Pourquoi la marge ne suffit pas :** `coast.rs` compose jusqu'à 27 bandes de torsions.
  - Chaque bande déplace de l'ordre de `strength·λ`, soit environ 0.25·λ.
  - Le commentaire `coast.rs:61-62` le reconnaît : le Jacobien de la composition n'est pas borné.
  - Sur 2.3 km, l'écart |q(x)−q(c)| contre |x−c| peut dépasser 200 m.
- **Conséquence :** des lacs et zones humides coupés au bord des provinces.
- **Correctif :** cette garde ne sert qu'au coût, donc on peut prendre une marge généreuse, par exemple `+ 10_000·scale`, sans aucun effet géométrique.

### B2. Recouvrement de bassins

Aucune exclusion mutuelle entre bassins. Un étang dont le centre tombe dans un grand lac reçoit son propre niveau, d'où un « lac dans le lac » entouré d'une arête sèche, mathématiquement correcte.

- Correctif déterministe et indépendant du bucket : supprimer une proposition si une proposition plus grande, avant gel, recouvre son centre. Le test est purement géométrique, sur les clés voisines à ±3.

### B3. Connectivité des fjords

Le tronc a un fond constant sur toute sa longueur et coupe en ligne droite reliefs et lignes de partage des eaux. Comme tout ce qui est sous 0 et sans réservoir devient océan (`sampler.rs:176-192`), un tronc qui finit dans un isthme, ou qui est bouché par une île (A1), s'affiche comme mer même sans connexion.

- Côté positif : la remap est un homéomorphisme, donc la connectivité en q est bien conservée en p.

### B4. Protection des rivières

La protection des rivières par la corde droite de l'atlas (`:606-616`) sous-estime l'écart des tronçons courbes. On ne peut pas y appeler `river_reach` : depuis `river_geometry::compile`, cela ferait une ré-entrée dans `OnceLock`, donc un interblocage. Il faut une marge plus large, sans cache.

### B5. Destinations

- `landforms::destinations` : si la racine 1 n'a pas trouvé de rive, `visit` reste le centre de la cellule côtière, qui n'est pas forcément sur l'eau.
- Un district coupé par A2 donne une destination vide.
- `river_geometry::exploration_destinations` et `inspection_target` sont corrects : identifiant de tronçon exigé, point mouillé, tri stable. Un micro-lac ou un fjord peut toutefois faire échouer des candidats ; ce n'est pas bloquant.

---

## C. Ce qui tient mathématiquement

- **Couverture des buckets de provinces :** la distance gnomonique (R·tanθ) est au moins égale à la corde, donc `extent = support·1.2/r` couvre bien le test `x²+y² ≤ R²`. Pour les îles, la boîte `2.2·long` contient `radial < 1.6`, puisque `outline ≥ 0.71`.
- **Halo des micro-lacs** (`neighbourhood`, `:387-393`) : correct. Projection radiale au plus 0.55·S, plus le support, et les bornes floor/floor incluent toutes les clés utiles.
- **Ordre déterministe :** tri par `id`, plafond à 64 vues déterministe, `planar_noise` indexé par clé signée. Le cache ne change que le coût, à condition que la callback d'environnement soit unique (c'est le cas : `local_water_at`).
- **Tranchées :** `min` de profils C1 qui retombent sur `natural` à `radial = 1`. Les branches partent de nœuds du tronc, donc elles sont connectées.
- **Lits de chenaux** (`sampler.rs:99-151`) :
  - Un chenal seul retrouve exactement `min(natural, carved)`.
  - Surface résultante `(L1 + w2·L2)/(1+w2)`, donc deux plans égaux restent égaux.
  - La profondeur signée est continue à `radial = 1`, en supposant `bank_height(hw) = level`.
  - Effet de bord : une berge sèche voisine rehausse le lit et réduit la profondeur.
- **Arêtes sèches entre micro-lacs :**
  - À l'égalité φ_a = φ_b, avec deux φ < 0, on a `ridge = 1`, donc la hauteur vaut au moins `crest + 0.25`, au-dessus des deux plans.
  - Continu au changement de propriétaire, y compris avec un troisième bassin : j'ai vérifié que `crest` et `ridge` sont égaux des deux côtés.
- **Franges :**
  - En φ = 0, la hauteur vaut `level + 0.18` : le lac est toujours fermé.
  - La plage φ∈(≈−0.3, 0) est sèche mais marquée zone humide, ce qui est voulu.
  - Les points φ > 0 sous le niveau ne sont pas inondés (filtre `:84`).
- **Repli marin :** les fjords sous 0 deviennent océan, les îles au-dessus de 0 retrouvent un biome terrestre (`:202-208`).
- **Caches paresseux :**
  - La compilation des provinces n'appelle que `atlas_height`.
  - La callback d'environnement n'entre ni dans le cache rivières ni dans les micro-lacs.
  - Le mutex est relâché pendant les callbacks.
  - Je ne vois pas de nouveau cycle. Le risque rayon + `OnceLock` sur `river_cache` existait déjà.

---

## D. Coûts à l'exécution

1. **Gel à froid :** environ 100 propositions dans la coque par bucket de 4 km. Chacune coûte une évaluation `environment`, et environ 30 % en coûtent 12 de plus, soit environ 450 appels par bucket.
   - Chaque appel fait une remap (jusqu'à 81 torsions), `geographic_height` (17 octaves de bruit), et `lake_geometry::supports` trois fois (`lake_divide_height`, `lake_datum`, `local_environment`).
   - À cause du halo, chaque proposition est gelée dans environ 5 à 6 buckets.
   - Correctif : stocker par bucket uniquement les propositions dont la clé est dans le bucket, sans halo, et fusionner les au plus 8 buckets voisins à l'échantillonnage. Le tri par `id` est déjà là.
2. **Rejet bon marché avant `geographic_height` :** tester d'abord `random(seed^0xdc15) >= density`, puis le poids de province, qui ne demande que la remap et le bucket. Cela rejette 45 % ou plus des propositions sans le calcul cher. Il faudrait aussi calculer `lake_datum` une seule fois par appel.
3. **Calculs en double entre threads :** plusieurs threads rayon peuvent calculer le même bucket en parallèle.
   - Correctif : insérer sous le mutex une entrée `Arc<OnceLock<Arc<[Basin]>>>`, puis l'initialiser hors mutex. C'est sans interblocage, car la callback ne revient pas dans l'atlas.
4. **Basse résolution :** à faible LOD, chaque sommet en province tombe dans un bucket différent. L'éviction FIFO à 1024 buckets s'emballe. Le cache par proposition (point 1) est la seule réponse qui ne fasse pas dépendre la géométrie de la caméra.
5. **Compilation des rivières :** `river_geometry::compile` (`:331`) passe par `natural_height`, donc par les micro-lacs. La compilation à froid des rivières déclenche ces gels.
6. **Contention :** un mutex global sur le chemin chaud. Un `RwLock` ou un découpage en shards serait mineur mais utile.

---

## E. Limites de conservation physique

Les fjords, îles et cuvettes sont des champs procéduraux appliqués **après** le drainage physique gelé.

- Le graphe `receiver`/`discharge`, les bilans des lacs, `ocean_volume` et `land_fraction` ne voient pas ces reliefs.
- Les îles n'ont ni ruissellement ni rivières.
- Une rivière peut entrer dans un tronc de fjord, devenir « océan », puis réapparaître en aval sur l'atlas. Les polylignes de `river_segments` sont dessinées au-dessus.
- Les micro-lacs n'ont ni apport, ni exutoire, ni évaporation. Le module le dit (« pas un budget »), mais `stats.lake_*` les ignore.
- Les seuils de déversement gelés ne sont plus garantis (A2).

C'est acceptable pour une reconstruction visuelle à topologie fixe, à condition de le documenter et de protéger à la compilation les objets physiques (réservoirs, seuils, tronçons).

---

## Évaluation finale

La structure est saine :
- adressage fixe, indépendant de la caméra et de l'ordre des requêtes ;
- couverture des buckets et halo corrects ;
- arêtes sèches et plans horizontaux mathématiquement cohérents ;
- transport q→p qui préserve la topologie ;
- pas de cycle de caches paresseux.

Ordre de correction recommandé, du plus important au moins important :
1. A1, la falaise île/fjord, qui est presque garantie par l'ordre partagé de la liste `coast`.
2. A2, les coupures binaires aux réservoirs et l'écrasement des seuils.
3. A3, l'inondation des chenaux par une donnée de micro-lac sèche.
4. A5 et A6, les petits artefacts géométriques.
5. A4, la petite discontinuité de mélange.
6. A7, les points latents.

Pour le coût, le gel à froid avec halo est à refaire en premier : cache par proposition et rejet bon marché.

Tous les correctifs proposés sont locaux, de l'ordre de 5 à 40 lignes chacun, et ne remettent pas l'approche en cause.
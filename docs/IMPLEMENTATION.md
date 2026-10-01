# État du prototype 0.6.4

Ce document décrit `prototype-0.6.4`, qui conserve l'évolution globale de 0.6, corrige les profils d'eau et les épaules des vallées, stabilise les flux GPU et publie l'érosion régionale par bandes de détail pour préserver l'enveloppe globale. Le [design complet](DESIGN_MONDE_MULTIECHELLE.md) reste le programme de recherche. Graine, paramètres et version déterminent l'identité du monde ; une ancienne sauvegarde n'est pas automatiquement régénérée.

## Compilation globale CPU

`world-core` construit six faces sphériques avec aires métriques, lithologie et soulèvement associés à des plaques procédurales. Le relief initial limite ses bandes à la résolution de l'atlas pour éviter de transformer des fréquences non résolues en fosses isolées. La mer est choisie par fraction océanique ou `water_volume_m3` dans la configuration.

L'évolution globale modifie réellement substrat et stock de sédiments : incision stream-power implicite pour `n = 1`, transport/dépôt, diffusion des versants et dispersion marine. Les petites dépressions numériques sont ouvertes par excavation, dont la masse est comptabilisée ; les grands bassins peuvent stocker de l'eau. Les rapports donnent soulèvement, masses érodées, stockées et exportées, changements d'altitude et résidus de bilan. Les champs sont encodés en float32 ; les accumulations globales et contrôles de masse utilisent float64.

`config.resolution` accepte 8 à 1024 cellules par face. L'aperçu HTTP expose séparément `render_resolution`, limité à 128 : augmenter l'atlas physique n'envoie pas des millions de cellules au renderer initial. La compilation globale reste CPU/Rayon ; CUDA intervient dans les régions.

## Mer, réservoirs et rivières

Le potentiel Priority-Flood est distinct du niveau d'eau physique. Les dépressions connectées reçoivent un réservoir avec identifiant, surface horizontale, volume, seuil et exutoire éventuel. Son budget suit `Q_entrée = Q_sortie + évaporation + infiltration + variation_stockage`, sur `lake_fill_years` — 25 ans par défaut. Un bassin fermé peut rester partiellement rempli ; une cellule sèche garde son altitude comme `water_m`.

Le sampler reconstruit le relief par interpolation cubique monotone bornée, puis ajoute des bandes fractales globales décroissantes avec déformation de domaine. Les nouvelles fréquences détaillent relief et côte sans déplacer les grands continents. Leur amplitude diminue près des chenaux pour préserver les profils hydrauliques. Les tronçons fluviaux dérivent du graphe et du débit ; leurs courbes relient les nœuds et leurs niveaux décroissent vers l'aval. Un trajet de potentiel montant ne devient pas automatiquement une rivière visible.

Le champ est évalué à une position sphérique fixe : `h = h_atlas + somme(a_k * bruit_k) + deltas_regionaux`. Les côtes sont ses intersections avec le niveau marin ; terre et fonds marins partagent ce champ continu. Les bandes suivent `lambda_k = R / (0.75 * N * 2^k)` et `a_k ∝ 2^(-0.85*k)` ; `lambda` désigne une échelle nominale de corrélation du bruit. Pour l'atlas 128 et le rayon 1 737 km, elles couvrent environ 18 km à 0,55 m. La lithologie module leur amplitude : autour de 1 km, l'enveloppe des coefficients vaut 3 à 6 m ; autour de 100 m, 0,4 à 0,9 m. La décroissance borne le déplacement des formes existantes, tandis que le maillage plus fin révèle leur contour. Les sommets communs restent identiques pour un même ensemble de régions actives ; l'activation d'une région plus fine ajoute son résidu par transition du renderer. Le prototype ne filtre pas encore les bandes selon l'empreinte d'un pixel et n'ajoute pas de spectre bathymétrique distinct.

Chaque `Sample` distingue sol, niveau hydraulique candidat et `water_body_id` : 0 pour l'océan, identifiant de lac, ou marqueur fluvial `1 << 52` compatible JSON. Les berges sèches reçoivent le niveau candidat utile au découpage ; les rivières exigent aussi que le point appartienne à leur section hydraulique. Lit et niveau restent associés au même tronçon, afin qu'un niveau amont n'inonde pas une vallée aval voisine. Le renderer découpe les triangles à la frontière sol/eau, avec plans horizontaux de lac ou inclinés pour les rivières ; il n'extrapole pas un plan fluvial aux sommets hors du chenal. Les biomes aquatiques ne débordent pas sur la terre émergée par interpolation.

## Érosion régionale CUDA et CPU

`world-erosion` implémente le même solveur float32 sur CPU et CUDA. Il transporte eau et sédiments par flux entre cellules, limite les sorties à l'eau disponible, puis applique érosion ou dépôt. Les limites exportent les flux. Les rapports rapprochent pluie, stockage et exports, ainsi que solides érodés, déposés, suspendus et exportés. Un masque protège l'altitude des cellules contraintes, sans interdire les échanges d'eau et de sédiments.

La version 2 borne `conductivité_effective * dt / espacement` à **0,12** pour stabiliser les échanges explicites et éviter le mode en damier. Le rapport expose `effective_conductivity_m_s` : cette limitation ajuste la loi réduite, sans sous-pas cachés. Le halo évolue avec l’intérieur ; seuls les ports hydrologiques imposent leur altitude. Aux sorties externes sans port, le delta d’érosion est prolongé avec une dérivée normale nulle, dans la charge hydraulique comme dans la pente sédimentaire. Cela évite qu’un sol extérieur figé devienne un mur après incision.

Le service prépare des régions canoniques aux niveaux **6 et 10**, avec **128 intervalles et un halo de 8 cellules**. La région 10 échantillonne le monde corrigé par sa région 6 avant sa propre érosion. Clés géographiques, identité du monde et version déterminent le stockage ; caméra, résolution d'une tuile et ordre des requêtes ne réensemencent pas le solveur.

Côtes, plans de lac et chenaux sont protégés. La publication extrait une bande de détail : `delta_détail = delta_brut − moyenne_locale(delta_brut)`. La moyenne utilise un noyau binomial séparable de 9 cellules, rayon 4, normalisé avec des poids nuls sur les cellules gelées. Un déplacement uniforme s'annule même près des ports ; il ne devient donc pas une cuvette carrée après le raccord. Les variations locales d'incision et de dépôt subsistent. Cette décomposition conserve l'enveloppe macroscopique du relief global, sans prétendre former une pyramide complète ou une restriction exacte.

`RegionPatch.residual_m` contient cette bande publiée ; son interpolation s'ajoute au champ analytique et conserve les fréquences plus fines que la grille. Une atténuation progressive sur 16 cellules s'annule avec une pente nulle à la frontière. L'intérieur peut être érodé : le figer créerait un cadre abrupt à sa première cellule active. Les petits niveaux de rendu omettent les bandes plus fines que leur couverture.

Un masque continu est réappliqué à la publication : les eaux sont figées, les corrections diminuent près du centre des chenaux et des berges. Il protège aussi une rivière passant entre des nœuds secs de la grille. Extraction de bande, protection et raccord modifient les résidus ; les bilans du solveur brut ne constituent donc pas un bilan conservatif exact de la surface publiée. La version régionale `multiscale6` invalide les artefacts antérieurs.

Les artefacts JSON vérifiés par BLAKE3 conservent hauteurs complètes du solveur, `raw_residual_m`, bande publiée, eau restante, sédiments suspendus et rapport. Ils sont rechargés au passage suivant. `Backend::Auto` essaie CUDA et revient au CPU avec `fallback_reason` explicite si pilote ou NVRTC manque. Le benchmark comparatif utilise `Backend::Cuda` et exige réellement CUDA.

## Exploration et API

Le renderer Rust/WASM utilise WebGPU ou WebGL2, positions haute/basse précision et profondeur inversée. Il affiche un ensemble complet de tuiles couvrant le champ de vision et une couronne de sécurité, au maximum 64 tuiles. La caméra suit leurs triangles, avec une inclinaison jusqu’à 35°. La résolution de 128 segments et les deux niveaux supplémentaires de détail sont conservés. Le préchargement vise le champ de vision du prochain zoom, limité à 64 tuiles supplémentaires, et le niveau parent.

L’éclairage rapproché suit la surface locale pour rendre les pentes lisibles sur toute la planète ; une transition douce retrouve le soleil orbital en altitude. Cette lumière d’inspection ne modifie pas le climat. `generator=analytic` sur une requête de tuile désactive les résidus régionaux pour comparer le champ de base au résultat érodé.

Le transport `format=postcard` utilise le codec partagé `world-core::tile_wire` : magic `WBT1`, puis Postcard 1.1.3 du `Tile` typé. Le WASM décode les octets directement ; JSON reste disponible par défaut pour inspection, et le mode `contracts` reste JSON. Sur une tuile 128 segments avec saison, le test API mesure **1 910 507 octets binaires contre 6 764 274 octets JSON**, sans perte de valeurs float32 ni d'identifiants hydrologiques.

## Stabilité du zoom côtier

Le champ analytique dépend de la direction globale, de l'atlas, de la graine et des paramètres ; la caméra n'intervient pas dans le bruit. Une nouvelle grille LOD échantillonne ce même champ. Les seuils 6 et 10 activent des résidus géographiques fixes et leur transition d'affichage, pas une nouvelle génération aléatoire. Au même niveau, une revisite réutilise exactement le paquet précédent. Les tests vérifient les points côtiers communs entre niveaux, threads et ordres de requêtes ; douze tuiles 128 segments aux niveaux 10/14/18 ont gardé un hash complet identique après parallélisation.

Sur la machine, le calcul des sommets de 96 tuiles passe d'une médiane de **34–38 ms à 11–12 ms**, et d'un p95 de **40–50 ms à 16–18 ms**. Ces mesures ne comprennent pas régions, sérialisation, transfert ni rendu. Les essais Windows/WebGPU avant modification reproduisent jusqu'à **5,4 s** de blocage au zoom et **6,4 s** au déplacement côtier, malgré 60 fps une fois la vue stabilisée. Le changement d'environnement en cours de vérification a empêché la mesure GPU finale : aucun chiffre de fluidité après correction n'est certifié ici. Les régressions de géométrie et les builds natif/WASM sont validés ; `window.worldBuilderPerf()` permet de mesurer la navigation réelle.

## Exploration et caches du navigateur

`web/tile-worker.js` charge les paquets, décode Postcard et prépare les maillages/normales dans une instance WASM indépendante, sans renderer. Un paquet interne `WVP1` transfère les sommets/indices prêts et les données exactes de collision. Les triangles visibles restent identiques ; seuls les sommets d'eau non référencés sont retirés. Le thread principal installe au maximum un paquet par image. Le scheduler borne la concurrence à trois fetchs et la somme des paquets en vol/prêts à six ; les demandes devenues inutiles sont annulées. Monde, saison et exagération invalident les résultats d'une ancienne époque.

Un déplacement au même niveau conserve les buffers des tuiles communes et ne relance pas leur morph. Le vrai changement de niveau prépare la transition par tranches d'environ **4 ms**, avec un transfert GPU par image, puis bascule tout le jeu à la fois et démarre le morph de 350 ms. Le budget est une cible CPU : une allocation/transmission individuelle peut le dépasser. Les contributions de normales ne sont partagées qu'aux frontières ; les maillages ne sont plus reconstruits en bloc.

Le cache LRU du viewer vise **768 Mio CPU et 768 Mio GPU**, comptés en octets ; les jeux actif et en préparation sont protégés jusqu'au basculement. Un dépassement temporaire est possible si tous les objets sont protégés. Ces compteurs couvrent les tuiles, pas le globe, l'atlas, la mémoire réservée par WASM ni celle du pilote. Le worker conserve **64 Mio** de paquets bruts et `CacheStorage` jusqu'à **384 Mio / 256 entrées**, disponibles après rechargement. Les écritures disque en attente sont plafonnées à quatre. Le préchargement de nouvelles tuiles hors niveau courant s'arrête à 80 % du budget CPU du viewer, pour éviter une boucle de préparation/éviction ; les tuiles du niveau demandé gardent la priorité. Les erreurs de quota ne bloquent pas la navigation. `window.worldBuilderPerf()` expose temps CPU, occupations, uploads, réutilisations et hits du worker ; `view_info()` fournit les niveaux actif/demandé, tuiles dessinées et transition en attente.

## Réutilisation et mesures du serveur

Les paquets de tuiles ont un cache RAM **LRU de 256 Mio, au maximum 512 entrées**. Un accès récent protège une tuile de l'éviction ; `WB_TILE_CACHE_MIB` permet de régler la capacité entre 16 et 2048 Mio. Monde, coordonnées, segments, saison, générateur et format font partie de la clé. JSON et Postcard restent distincts. Un cache disque LRU reconstructible de **1 Gio**, dans `data/tile-cache/`, survit au redémarrage ; chaque paquet est vérifié par BLAKE3 avant réutilisation. Les versions du générateur, du solveur régional et du codec de cache déterminent son espace de clés. Une corruption de paquet provoque un recalcul ; elle ne modifie pas l'atlas sauvegardé.

Les requêtes simultanées d'une même clé partagent un seul calcul. Les quatre workers gardent leur permis jusqu'à la fin du travail CPU/CUDA, même si le navigateur annule un fetch devenu inutile ; le résultat rejoint quand même les caches. Cela empêche les calculs abandonnés de dépasser la limite de concurrence. Le cache régional garde **128 artefacts LRU**, avec un verrou par région ; une région froide ne bloque plus les accès aux autres régions déjà calculées. Ses champs, masques et équations sont inchangés.

`GET /api/performance` expose occupation, évictions, hits RAM/disque, calculs, requêtes regroupées et temps cumulés d'attente/calcul depuis le lancement. Chaque réponse de tuile fournit `X-World-Tile-Cache: memory|disk|coalesced|generated` et `Server-Timing` avec `queue`, `compute`, `total` en millisecondes. Ces mesures concernent le serveur avant transfert et décodage/rendu navigateur ; elles distinguent une première exploration d'une revisite. Les tests vérifient l'identité exacte des octets après regroupement, changement d'ordre, éviction et rechargement disque, ainsi que la fin du calcul après annulation.

Ces optimisations conservent `prototype-0.6.4` et l'identité des mondes : aucune nouvelle planète ni nouvelle érosion n'est nécessaire. Supprimer `data/tile-cache/` efface seulement des paquets recalculables ; `data/worlds/<id>/regions/` conserve les artefacts physiques canoniques.

## Services complémentaires

Les saisons utilisent insolation et inertie simplifiées ; les débits restent annuels. La couche humaine répartit une population sur des sites agrariens favorables puis trace des routes A* sur la terre accessible. Îles et connexions impossibles sont signalées ; les bâtiments restent des symboles.

| Endpoint | Usage |
| --- | --- |
| `POST /api/worlds` ; `GET /api/jobs/{id}` | Lancer et suivre une génération globale. |
| `GET /api/world/latest/preview?resolution=128` | Aperçu borné, distinct de l'atlas. |
| `GET /api/worlds/{id}/tiles/{face}/{level}/{x}/{y}?segments=64` | Relief spatial avec régions persistantes au zoom. |
| Même endpoint avec `&format=postcard` | Transport binaire vers le renderer WASM. |
| `GET /api/worlds/{id}/lakes` | Réservoirs, niveaux, volumes, débits et positions. |
| `GET /api/worlds/{id}/regions/reports` | Backends, temps et bilans des régions calculées. |
| `GET /api/worlds/{id}/reports` | Validation et statistiques globales. |
| `GET /api/performance` | Caches, concurrence et temps cumulés du serveur. |
| `GET /api/worlds/{id}/human` | Population, localités et routes. |

`generator=contracts`, `/families/...` et `world-app refine-demo` gardent l'expérience de subdivision géométrique. Ses certificats portent sur moyennes, moments, frontières et quantification au centimètre dans la quadrature du modèle. Ce générateur peut produire des losanges artificiels ; il est masqué dans l'interface principale et ne certifie ni drainage, ni côtes, ni masse solide.

## Limites physiques et garanties

- Les réservoirs sont calculés par dépression connectée, sans hiérarchie complète des dépressions imbriquées ni cycle Fill–Spill–Merge temporel.
- Les lois globales de capacité sédimentaire et de dépôt sont des fermetures simplifiées avec limitation de l'accumulation. Le code n'importe pas FastScape Fortran et n'en reproduit pas l'ensemble du modèle.
- Le solveur régional est un modèle réduit de ruissellement et d'érosion à pluie finie, sans Navier–Stokes. Sa durée en secondes n'est pas raccordée à l'horloge géologique globale ; ses coefficients ne sont pas calibrés sur des terrains observés.
- Ses bilans concernent une grille à espacement local constant. Les ports hydrologiques sont protégés, mais les flux régionaux ne sont pas couplés dans une simulation planétaire continue.
- Le fractal est déterministe et les échantillons communs se raccordent ; l'ensemble des tuiles n'a pas de conservation exacte des moments, de la masse ou du drainage sous réduction d'échelle. Érosion locale et maillage ne recalculent pas les grands bassins.
- Climat et biomes restent paramétriques, sans GCM, advection conservatrice ni cycle annuel hydrologique couplé. L'érosion sous-marine est incomplète ; aucune fidélité géologique quantitative n'est démontrée.
- Rapports numériques et captures vérifient fonctionnement et invariants ; ils ne suffisent pas à établir le réalisme des paysages.

L'incision implicite s'appuie sur [Braun et Willett (2013)](https://doi.org/10.1016/j.geomorph.2012.10.008). La [publication Fill–Spill–Merge](https://esurf.copernicus.org/articles/9/105/2021/) décrit la hiérarchie encore absente ; la [documentation FastScapeLib](https://fastscape.org/fastscapelib-fortran/) situe les extensions qui dépassent ce prototype.

## Mesures et vérification

Les tests couvrent géométrie, drainage, bilans d'eau et de sédiments, niveaux de lac, profils fluviaux, déterminisme, saisons, raccords, résidus régionaux, population, routes, persistance et caméra. Une régression vérifie qu'un delta GPU conserve la variation analytique à des positions situées dans une même maille. Exécuter `cargo test --workspace` pour l'état présent ; le nombre de tests évolue avec les travaux.

Mesures du **1er octobre 2026**, Rust release, RTX 3090, solveur v2, mêmes champs synthétiques et **160 pas** CPU/CUDA, compilation Rust exclue :

| Grille | CUDA après initialisation | Référence CPU | Écart maximal CPU/CUDA |
| --- | ---: | ---: | ---: |
| 257 × 257 | 5,93 ms | 1 148,95 ms | 0 m |
| 1025 × 1025 | 82,75 ms | 23 441,4 ms | 0 m |

Pour 1025², les résidus relatifs mesurés étaient environ **4,7 × 10⁻⁸ pour les solides** et **6,0 × 10⁻⁸ pour l'eau**. L'initialisation NVRTC/pilote a coûté **0,415 s** ; un premier démarrage peut être plus lent. Ces chiffres incluent solveur et transferts, mais excluent échantillonnage du monde, génération de régions, JSON, disque et rendu.

Benchmark CPU global 0.6, atlas **128**, graine **1337**, paramètres par défaut, machine occupée par les travaux simultanés : **98 304 cellules en 10,81 s**, 10 000 samples en **45,57 ms**, tuiles 64 segments **p50 11,38 ms / p95 19,29 ms**. Le monde `d532defabe6638f4950c` possède 16 lacs et 90 cellules mouillées, profondeur médiane 53,58 m, maximale 429,18 m. Sur 2 millions d'années et 24 pas, l'altitude change de 1283,15 m RMS ; le résidu de masse solide est **1,46 × 10⁻¹⁴**, celui du budget d'eau **2,20 × 10⁻¹⁵**. Ces résultats vérifient le modèle discret ; ils ne calibrent pas sa géologie.

La génération finale 0.6.4, avec les mêmes paramètres physiques, a pris **8,14 s** : monde `00c0251e88d34d575f14`, 16 lacs, validation globale réussie. Les révisions suivantes de 0.6 corrigent l'échantillonnage et les raccords ; elles conservent cet atlas physique. La vérification initiale comptait **67 tests réussis** ; la révision des caches et du renderer compte **75 tests Rust et 3 tests JavaScript**, notamment invariance des triangles visibles, coordonnées canoniques des normales, déterminisme parallèle et bornes du scheduler. Les trois tests du solveur passent aussi avec `--no-default-features`, sans CUDA.

Le contrôle visuel de cette révision a comparé le sommet à 30 km et 1 km, les rivières et berges à 1 km, une vallée à 3 km et 1 km, puis la même côte à 30 km et 100 m. Le front rectangulaire de dépôt et le faux bras ont disparu ; les détails côtiers apparaissent sans trou ni transparence. Les matériaux restent simples, le microrelief discret à ×1 et certains tronçons ou certaines rives assez rectilignes. Les comparaisons CPU/CUDA sont des mesures ponctuelles sur cette machine de développement.

Reproduire sous WSL :

```bash
LD_LIBRARY_PATH="/usr/lib/wsl/lib:/usr/local/cuda/lib64:${LD_LIBRARY_PATH:-}" \
  cargo run --release -p world-erosion --example compare -- 257
LD_LIBRARY_PATH="/usr/lib/wsl/lib:/usr/local/cuda/lib64:${LD_LIBRARY_PATH:-}" \
  cargo run --release -p world-erosion --example compare -- 1025
cargo run --release -p world-core --example benchmark -- 128 1337
cargo test --workspace
```

Sous Windows, installer pilote et CUDA Toolkit/NVRTC, puis ajouter son dossier `bin` à `PATH` avant la même commande `cargo run`. `scripts/run.sh` prépare déjà les chemins WSL pour le serveur. CUDA et rendu navigateur restent deux capacités séparées : WebGL2 a servi aux contrôles visuels automatisés ; les mesures CUDA ne qualifient pas les performances WebGPU du navigateur. Aucun CI/CD complexe n'est requis pour ce prototype.

# État du prototype 0.8.0

Ce document décrit `prototype-0.8.0` : érosion par défaut réduite, compilation globale accélérée, détail côtier irrégulier fixe, protection lacustre continue, profils fluviaux partagés et visualiseur jusqu’au niveau 24. Le [design complet](DESIGN_MONDE_MULTIECHELLE.md) reste le programme de recherche. Les sauvegardes précédentes sont conservées ; cette révision nécessite une génération explicite pour bénéficier du nouveau champ.

La graine utilisateur 2386084681 en atlas 512² × 6 est préparée en 20,12 s ; validation et sauvegarde portent la commande complète à 24,98 s. Le climat fixe est précalculé une fois, les parcours Priority-Flood inutiles sont supprimés et les tableaux de drainage sont compacts. À paramètres anciens identiques, les premières optimisations conservent exactement les statistiques et le bilan d’érosion mesurés. Les valeurs par défaut passent ensuite de 24 étapes/2 millions d’années à 8 étapes/350 000 ans, réduisant le changement d’altitude RMS de 71 %. Les [mesures 0.8](PERFORMANCE_0_8.md) distinguent ces deux gains et les contrôles navigateur.

## Compilation globale CPU

`world-core` construit six faces sphériques avec aires métriques, lithologie et soulèvement associés à des plaques procédurales. Le relief initial limite ses bandes à la résolution de l'atlas pour éviter de transformer des fréquences non résolues en fosses isolées. La mer est choisie par fraction océanique ou `water_volume_m3` dans la configuration.

L'évolution globale modifie réellement substrat et stock de sédiments : incision stream-power implicite pour `n = 1`, transport/dépôt, diffusion des versants et dispersion marine. Les petites dépressions numériques sont ouvertes par excavation, dont la masse est comptabilisée ; les grands bassins peuvent stocker de l'eau. Les rapports donnent soulèvement, masses érodées, stockées et exportées, changements d'altitude et résidus de bilan. Les champs sont encodés en float32 ; les accumulations globales et contrôles de masse utilisent float64.

`config.resolution` accepte 8 à 1024 cellules par face. L'aperçu HTTP expose séparément `render_resolution`, limité à 128 : augmenter l'atlas physique n'envoie pas des millions de cellules au renderer initial. La compilation globale reste CPU/Rayon ; CUDA intervient dans les régions.

## Mer, réservoirs et rivières

Le potentiel Priority-Flood est distinct du niveau d'eau physique. Les dépressions connectées reçoivent un réservoir avec identifiant, surface horizontale, volume, seuil et exutoire éventuel. Son budget suit `Q_entrée = Q_sortie + évaporation + infiltration + variation_stockage`, sur `lake_fill_years` — 25 ans par défaut. Un bassin fermé peut rester partiellement rempli ; une cellule sèche garde son altitude comme `water_m`.

La détection retient les bassins dépassant 1 m de profondeur, puis rattache leurs franges connectées jusqu’à une profondeur de 1 mm. Le plan d’un lac appartient à un domaine fixe construit à partir des cellules de son bassin, y compris les franges sèches. Le domaine est une union de voisinages métriques, indépendante des axes des faces et du quad de rendu ; une berge continue retient les plans différents quand le DEM grossier a perdu leur divide. La rive reste l’intersection du sol avec le plan horizontal. Les vrais petits bassins restent sous-résolus à faible résolution d’atlas.

Le niveau marin est prolongé sur le sous-niveau zéro hors des bassins physiques connus. Les bassins enregistrés conservent leur propre plan, y compris sur leurs franges sèches. La protection du détail lacustre dépend d’un poids continu des cellules actives et conserve les fonds des cellules mouillées. La sélection hydraulique utilise le même domaine métrique que les berges ; elle ne dépend plus de l’apparition d’un coin de quad.

Le sampler reconstruit l’atlas par interpolation cubique monotone. Une composition fixe de twists sphériques ajoute des courbes côtières sur plusieurs bandes jusqu’à `coast_reference_spacing_m` (0,25 m par défaut). `coast_detail_ratio` règle son intensité (0,30 par défaut). Chaque bande utilise trois axes semés, des fréquences indépendantes et des points de bruit irréguliers, supprimant le rythme de la grille régulière précédente. Le détail vertical est borné par `0,8 × |h_côte|` : il garde le signe du champ côtier. Cette règle évite qu’une nouvelle bande verticale transforme une petite crête littorale en île ou en mare. Le maillage grossier peut encore manquer une forme plus petite que ses triangles ; chaque twist tourne autour d’un axe avec un angle dépendant uniquement de sa coordonnée axiale, conservée par la rotation. Son inverse est la rotation opposée ; la composition conserve la connectivité analytique du champ remappé avant les contraintes lacustres et l’incision. Cela ne prouve pas la topologie des polygones sous-échantillonnés.

Les bandes de relief suivent `lambda_k = R / (0.75 × N × 2^k)` et `a_k ∝ 2^(-0.85k)`, jusqu’à 0,5 m de corrélation nominale. Tous les niveaux évaluent les mêmes bandes aux mêmes positions : ni caméra, ni ordre de requête, ni nombre de threads ne changent le champ. Les sommets communs sont identiques. Le détail est construit avant l’incision des chenaux ; enlever une bande négative ne peut plus remonter artificiellement un fond de vallée.

`river_geometry` compile une fois le catalogue des courbes, nœuds hydrauliques, largeurs et profondeurs. Le sampler et les tronçons de l’API utilisent les mêmes géométries et profils longitudinaux. Ces profils sont ajustés au terrain naturel sur 128 segments fixes, puis leurs niveaux sont propagés sur le graphe sans abaisser les contrôles lacustres ou marins. Trois passes supplémentaires ajustent les profils aux vallées voisines, au centre et aux deux bords mouillés. Les recouvrements utilisent une partition continue commune au lit et au potentiel hydraulique signé des berges ; changer de propriétaire ne remplace plus les hauteurs. Les sections isolées et les plans communs sont préservés. Ce raccord géométrique ne résout pas les équations hydrodynamiques et son remodelage ne possède pas de bilan de sédiments. Les embouchures sont découpées à la côte naturelle, le niveau marin vaut zéro, et seuls les vrais contrôles sortants des lacs produisent un exutoire. La section associe lit doux, berge et plaine inondable avant le raccord au versant. Le zoom orbital affiche au plus 256 reaches principaux ; ce plafond n’élimine aucun chenal dans les tuiles locales.

La génération corrige trois défauts du solveur précédent : l’incision côtière utilisait le fond sous-marin comme niveau aval ; le dépôt trop immédiat refermait les vallées ; les faibles dépressions artificielles s’accumulaient. Le dépôt est maintenant réparti sur une longueur de transport, les drains reçoivent une érosion latérale conservatrice, et `lake_min_basin_depth_m` (8 m) ouvre réellement les petites cuvettes. Les volumes excavés et les transferts de sédiments restent dans le bilan.

Chaque `Sample` distingue sol, niveau hydraulique candidat et `water_body_id` : 0 pour l'océan, identifiant de lac, ou marqueur fluvial `1 << 52` compatible JSON. Les berges sèches reçoivent le niveau candidat utile au découpage ; les rivières exigent aussi que le point appartienne à leur section hydraulique. Lit et potentiel hydraulique sont reconstruits avec la même partition ; le propriétaire est une métadonnée, pas un sélecteur de hauteur. Le catalogue ajuste d'abord les profils pour éviter qu'un niveau amont ne reste suspendu au-dessus d'une vallée aval voisine. Le renderer découpe les triangles à la frontière sol/eau, avec plans horizontaux de lac ou inclinés pour les rivières ; il n'extrapole pas un plan fluvial aux sommets hors du chenal. Les biomes aquatiques ne débordent pas sur la terre émergée par interpolation.

## Érosion régionale CUDA et CPU

`world-erosion` implémente le même solveur float32 sur CPU et CUDA. Il transporte eau et sédiments par flux entre cellules, limite les sorties à l'eau disponible, puis applique érosion ou dépôt. Les limites exportent les flux. Les rapports rapprochent pluie, stockage et exports, ainsi que solides érodés, déposés, suspendus et exportés. Un masque protège l'altitude des cellules contraintes, sans interdire les échanges d'eau et de sédiments.

La version 2 borne `conductivité_effective * dt / espacement` à **0,12** pour stabiliser les échanges explicites et éviter le mode en damier. Le rapport expose `effective_conductivity_m_s` : cette limitation ajuste la loi réduite, sans sous-pas cachés. Le halo évolue avec l’intérieur ; seuls les ports hydrologiques imposent leur altitude. Aux sorties externes sans port, le delta d’érosion est prolongé avec une dérivée normale nulle, dans la charge hydraulique comme dans la pente sédimentaire. Cela évite qu’un sol extérieur figé devienne un mur après incision.

Le service prépare des régions canoniques aux niveaux **6 et 10**, avec **128 intervalles et un halo de 8 cellules**. La région 10 échantillonne le monde corrigé par sa région 6 avant sa propre érosion. Clés géographiques, identité du monde et version déterminent le stockage ; caméra, résolution d'une tuile et ordre des requêtes ne réensemencent pas le solveur.

Côtes, plans de lac et chenaux sont protégés. La publication extrait une bande de détail : `delta_détail = delta_brut − moyenne_locale(delta_brut)`. La moyenne utilise un noyau binomial séparable de 9 cellules, rayon 4, normalisé avec des poids nuls sur les cellules gelées. Un déplacement uniforme s'annule même près des ports ; il ne devient donc pas une cuvette carrée après le raccord. Les variations locales d'incision et de dépôt subsistent. Cette décomposition conserve l'enveloppe macroscopique du relief global, sans prétendre former une pyramide complète ou une restriction exacte.

`RegionPatch.residual_m` contient cette bande publiée ; son interpolation s'ajoute au champ analytique et conserve les fréquences plus fines que la grille. Une atténuation progressive sur 16 cellules s'annule avec une pente nulle à la frontière. L'intérieur peut être érodé : le figer créerait un cadre abrupt à sa première cellule active. Les petits niveaux de rendu omettent les bandes plus fines que leur couverture.

Un masque continu est réappliqué à la publication : les eaux sont figées, les corrections diminuent près du centre des chenaux et des berges. Il protège aussi une rivière passant entre des nœuds secs de la grille. Extraction de bande, protection et raccord modifient les résidus ; les bilans du solveur brut ne constituent donc pas un bilan conservatif exact de la surface publiée. La version régionale `multiscale6` invalide les artefacts antérieurs.

Les artefacts JSON vérifiés par BLAKE3 conservent hauteurs complètes du solveur, `raw_residual_m`, bande publiée, eau restante, sédiments suspendus et rapport. Ils sont rechargés au passage suivant. `Backend::Auto` essaie CUDA et revient au CPU avec `fallback_reason` explicite si pilote ou NVRTC manque. Le benchmark comparatif utilise `Backend::Cuda` et exige réellement CUDA.

## Exploration et API

Le renderer Rust/WASM utilise WebGPU ou WebGL2, positions haute/basse précision et profondeur inversée. Six racines canoniques restent résidentes. Une coupe quadtree couvre chaque face sans superposer parent et descendants : le parent est remplacé seulement lorsque ses quatre enfants sont prêts. Un voisin absent conserve sa feuille grossière, sans retour global au globe initial. Le choix de niveau tient compte de l’empreinte des sommets à l’écran et du champ visible avec une couronne de sécurité ; les racines gardent 128 segments et les tuiles diffusées 64 segments, avec un niveau cible supplémentaire.

L’éclairage rapproché suit la surface locale pour rendre les pentes lisibles sur toute la planète ; une transition douce retrouve le soleil orbital en altitude. Cette lumière d’inspection ne modifie pas le climat. `generator=coherent` est le profil navigateur ; `analytic` reste son alias API. `spatial` permet de comparer les résidus régionaux au champ de base.

Le transport `format=postcard` utilise le codec partagé `world-core::tile_wire` : magic `WBT1`, puis Postcard 1.1.3 du `Tile` typé. Le WASM décode les octets directement ; JSON reste disponible par défaut pour inspection, et le mode `contracts` reste JSON. Sur une tuile 128 segments avec saison, le test API mesure **1 910 507 octets binaires contre 6 764 274 octets JSON**, sans perte de valeurs float32 ni d'identifiants hydrologiques.

## Stabilité du zoom côtier

Le profil navigateur `generator=coherent` évalue exactement le même champ de la vue orbitale au sol. L’aperçu publie les six grilles de sommets L0 (`render_grid=vertices`), plutôt que des centres de cellules réinterpolés. Les tests comparent tous les sommets de l’aperçu aux tuiles racines, les sommets communs des quatre premiers enfants, et les paquets binaires aux seuils 5/6 et 9/10. Caméra, déplacement et ordre de requêtes ne changent pas le champ. Les anciens paquets sont séparés par la révision de surface `spherical-coast-7`.

Benchmark historique du calcul des sommets : sur cette machine, 96 tuiles passent d'une médiane de **34–38 ms à 11–12 ms**, et d'un p95 de **40–50 ms à 16–18 ms**. Ces mesures ne comprennent pas régions, sérialisation, transfert ni rendu. Les essais Windows/WebGPU avant modification reproduisent jusqu'à **5,4 s** de blocage au zoom et **6,4 s** au déplacement côtier, malgré 60 fps une fois la vue stabilisée. Les mesures navigateur actuelles sont détaillées dans [le rapport de streaming 0.7](PERFORMANCE_0_7.md). Les régressions de géométrie et les builds natif/WASM sont validés ; `window.worldBuilderPerf()` permet de mesurer la navigation réelle.

## Exploration et caches du navigateur

`web/tile-worker.js` charge les paquets, décode Postcard et prépare les maillages/normales dans une instance WASM indépendante, sans renderer. Un paquet interne `WVP1` transfère les sommets/indices prêts et les données exactes de collision. Les triangles visibles restent identiques ; seuls les sommets d'eau non référencés sont retirés. Le thread principal installe au maximum un paquet par image. Le scheduler borne la concurrence à trois fetchs et la somme des paquets en vol/prêts à six ; les demandes devenues inutiles sont annulées. Monde, saison et exagération invalident les résultats d'une ancienne époque.

Un déplacement au même niveau conserve les buffers disponibles. Les sommets parent sont retrouvés par indices imbriqués et interpolation sur leurs triangles ; seules les bordures demandent les calculs de raccord. Les attributs GPU occupent 72 octets statiques et 44 octets dynamiques par sommet, soit 116 octets hors indices. Les normales partagées sont recalculées aussi lors du déraffinement. Toutes les feuilles de la partition sont dessinées : la sélection du champ visible ne doit pas découper une falaise exagérée. Chaque batch indépendant dispose d’une horloge de morph de 160 ms, avec une durée unique partagée par Rust et le shader ; le déraffinement suit le trajet inverse avant de remplacer les enfants. Les bordures fines restent sur la géométrie du voisin grossier jusqu’à son raffinement. Une transition attend la fin du morph des voisins qu’elle doit modifier. Le shell océan couvre les supports parent/enfant et interpole leur profondeur, y compris lorsque l’un devient sec ; il respecte les domaines sans connexion marine. Les chenaux sont masqués avec la couche rivières, désactivée par défaut comme les routes.

La préparation adapte son budget entre mouvement et arrêt, avec une échéance partagée entre planification et mises à jour. Plusieurs quartets peuvent progresser ensemble quand leurs voisinages sont disjoints ; leur publication reste atomique. Une allocation ou installation individuelle peut encore dépasser la cible. Le cache du viewer vise **768 Mio CPU et 768 Mio GPU**. Les feuilles actives, leurs parents immédiats, la lignée centrale, les groupes en préparation et les prochains quartets sont protégés. Le JavaScript synchronise les clés résidentes avec le renderer et invalide immédiatement les paquets lors d’un changement d’époque. Les buffers GPU évincés sont détruits explicitement. La coupe indispensable peut dépasser le budget, y compris à caméra immobile. Sous pression, les groupes hors champ sont déraffinés en priorité, sans morph invisible. Le champ visible est revérifié avant publication : un groupe redevenu visible reprend la transition normale. Les demandes concernent la frontière active : enfants immédiats à raffiner et parents des quartets à déraffiner. Redemander toute l’ascendance à chaque image provoquerait une boucle d’éviction/rechargement.

Le worker conserve **128 Mio** de paquets bruts et `CacheStorage` jusqu’à **384 Mio / 2 048 entrées** ; les écritures en attente sont plafonnées à quatre. Les plafonds en octets restent prioritaires : les anciennes limites de nombre évinçaient trop vite les petites tuiles à 64 segments. Une prédiction de mouvement yaw/pitch/log-altitude à **350 ms** réchauffe au plus 64 nouvelles tuiles, sans modifier la caméra ni le champ. Les ancêtres utiles et tuiles visibles passent avant les prévisions ; le préchargement cesse à 80 % du budget CPU. Les compteurs comprennent les tuiles racines, mais pas l’aperçu source conservé, les réserves de mémoire WASM ou le pilote. `window.worldBuilderPerf()` expose temps CPU, occupations, uploads, réutilisations et hits du worker ; `forecast_requests` mesure les demandes anticipées, `view_info()` la coupe active et sa cible.

## Réutilisation et mesures du serveur

Les paquets de tuiles ont un cache RAM **LRU de 256 Mio, au maximum 2 048 entrées**. Un accès récent protège une tuile de l'éviction ; `WB_TILE_CACHE_MIB` permet de régler la capacité entre 16 et 2048 Mio. Monde, coordonnées, segments, saison, générateur et format font partie de la clé. JSON et Postcard restent distincts. Un cache disque LRU reconstructible de **1 Gio**, dans `data/tile-cache/`, survit au redémarrage ; chaque paquet est vérifié par BLAKE3 avant réutilisation. Les versions du générateur, du solveur régional et du codec de cache déterminent son espace de clés. Une corruption de paquet provoque un recalcul ; elle ne modifie pas l'atlas sauvegardé.

Les requêtes simultanées d'une même clé partagent un seul calcul. Les quatre workers gardent leur permis jusqu'à la fin du travail CPU/CUDA, même si le navigateur annule un fetch devenu inutile ; le résultat rejoint quand même les caches. Cela empêche les calculs abandonnés de dépasser la limite de concurrence. Le cache régional garde **128 artefacts LRU**, avec un verrou par région ; une région froide ne bloque plus les accès aux autres régions déjà calculées. Ses champs, masques et équations sont inchangés.

`GET /api/performance` expose occupation, évictions, hits RAM/disque, calculs, requêtes regroupées et temps cumulés d'attente/calcul depuis le lancement. Chaque réponse de tuile fournit `X-World-Tile-Cache: memory|disk|coalesced|generated` et `Server-Timing` avec `queue`, `compute`, `total` en millisecondes. Ces mesures concernent le serveur avant transfert et décodage/rendu navigateur ; elles distinguent une première exploration d'une revisite. Les tests vérifient l'identité exacte des octets après regroupement, changement d'ordre, éviction et rechargement disque, ainsi que la fin du calcul après annulation.

La validation d’identité refuse les atlas d’un ancien générateur ; régénérer explicitement la graine avec la nouvelle version. Les fichiers d’anciens mondes restent conservés. La révision de surface `spherical-coast-7` sépare aussi les paquets de rendu précédents. Supprimer `data/tile-cache/` efface seulement des paquets recalculables ; `data/worlds/<id>/regions/` conserve les artefacts physiques canoniques.

## Services complémentaires

Les saisons utilisent insolation et inertie simplifiées ; les débits restent annuels. La couche humaine répartit une population sur des sites agrariens favorables puis trace des routes A* sur la terre accessible. Îles et connexions impossibles sont signalées ; les bâtiments restent des symboles.

| Endpoint | Usage |
| --- | --- |
| `POST /api/worlds` ; `GET /api/jobs/{id}` | Lancer et suivre une génération globale. |
| `GET /api/world/latest/preview?resolution=128` | Aperçu borné, distinct de l'atlas. |
| `GET /api/worlds/{id}/tiles/{face}/{level}/{x}/{y}?segments=64` | Champ cohérent par défaut ; régions avec `generator=spatial`. |
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

La génération finale 0.6.4, avec les mêmes paramètres physiques, a pris **8,14 s** : monde `00c0251e88d34d575f14`, 16 lacs, validation globale réussie. Les révisions suivantes de 0.6 corrigent l'échantillonnage et les raccords ; elles conservent cet atlas physique. La vérification initiale comptait **67 tests réussis**. Les régressions ajoutées couvrent notamment les racines exactes, la partition complète du globe, les transitions inverses, l’ordre d’arrivée des quartets et la priorité des paquets prêts. Les trois tests du solveur passent aussi avec `--no-default-features`, sans CUDA.

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

Sous Windows, installer pilote et CUDA Toolkit/NVRTC, puis ajouter son dossier `bin` à `PATH` avant la même commande `cargo run`. `scripts/run.sh` prépare déjà les chemins WSL pour le serveur. CUDA et rendu navigateur restent deux capacités séparées. Les contrôles de zoom du 3 octobre utilisent Chrome Windows et son adaptateur WebGPU NVIDIA Ampere ; les mesures CUDA concernent le solveur régional. Aucun CI/CD complexe n'est requis pour ce prototype.

### Mesures historiques 0.6.5 — 3 octobre 2026

Revue effectuée par **Claude Opus 5.5 en CLI 2.1.288**, en lecture seule, puis corrections : champ unique dès l’aperçu, partition sans superposition, travail limité aux bordures, préparation hors champ accélérée, voisinages partagés et suppression des rechargements d’ascendance. La génération explicite de la graine 617869129 produit `5ab160a193b596fd4e26` en **5,91 s** ; ses 98 304 altitudes d’atlas sont bit à bit celles du monde précédent. Les faux découpages de réservoir fusionnent ; il reste 15 lacs.

Chrome Windows, adaptateur WebGPU NVIDIA Ampere/RTX 3090, 1280×900 : les premiers paliers de 7R à 150 km donnent un **p95 de 16,8 ms** et un maximum de 33,4 ms. Le test final de 100 pas de zoom jusqu’à 75 m atteint le LOD 17 après environ 25 s supplémentaires de raffinement. Le champ et la caméra restent continus pendant cette progression ; le détail maximal n’est pas immédiat. La coupe finit avec 1 537 installations puis **aucune nouvelle installation durant 5 s** à caméra immobile, `in_flight=0` et `surface_fallbacks=0`. Les caches étaient déjà partiellement chauds. Un essai précédent mesurait p95 16,8 ms/p99 25,9 ms/max 50,1 ms et quelques tâches principales de 50–62 ms ; les installations ne respectent donc pas toutes une cible de 4 ms. Ces mesures ponctuelles ne certifient pas toutes les configurations ou trajectoires.

Les budgets sont souples : la coupe profonde et ses parents ont atteint 1,62 Go CPU et 1,05 Go GPU dans cet essai. Les paquets worker, la mémoire WASM et le pilote s’ajoutent. Une deuxième revue Opus 5.5 a confirmé le blocage inverse provoqué par le filtre mémoire JavaScript : les parents requis au déraffinement doivent rester demandés même au-dessus du niveau cible. Seules les demandes anticipées sont suspendues sous pression. Au retour de 75 m à 7R, la coupe revient bien au niveau 1 ; la mémoire descend à **766 Mio CPU / 502 Mio GPU**, sans requête restante. La fin du déraffinement prend environ 50 s après le dézoom rapide, pendant lesquels le globe reste dessiné. Le budget affiché ne constitue pas une borne dure.

Validation historique 0.6.5 : **93 tests Rust**, **8 tests JavaScript**, `cargo fmt --check`, build natif release et build WASM release réussis.

Captures WebGPU de la même direction côtière : [900 km](screenshots/coherent-coast-900km.png), [75 m après stabilisation](screenshots/coherent-coast-75m.png). Le raccord des trois faces a aussi été parcouru à 150 km, puis avec inclinaison 35° et exagération ×30 ; aller-retour et inversion du zoom gardent une partition complète et `surface_fallbacks=0`.


## Vérification de la révision 0.7

Le 3 octobre 2026, génération release de la graine utilisateur `2386084681`, rayon 1 737 km, atlas 512² × 6, 25 étapes et 2 000 001 ans : 115 729 ms, 284 lacs / 3 334 cellules mouillées, contre 1 109 / 11 251 avant correction. Aucun cycle de drainage ; résidu relatif solide `2,26e-13`, hydrique du rapport sauvegardé `1,52e-9` (somme des débits float32), et `2,39e-14` dans les accumulations float64 de génération. Ces chiffres portent sur la génération globale, hors compilation Rust.

La [validation de l’érosion](EROSION_VALIDATION.md) et la [comparaison Burgmap](QUALITY_REFERENCE.md) distinguent érosion réelle et détail fractal. À 512, une cellule représente environ 4,91 km ; les vallons, tributaires et deltas simulés à quelques dizaines de mètres restent à intégrer par régions canoniques. Une subdivision plus fine n’apporte pas automatiquement cette physique.

L’[audit Sonnet 5.5](AUDIT_BUGS_PERFORMANCE.md) et la [revue Opus 5.5 du LOD](LOD_REVIEW.md) sont issus de la CLI réelle. Leurs hypothèses de performance sont distinguées des défauts démontrés ; les mesures navigateur complètent la lecture du code.

Les [mesures de streaming 0.7](PERFORMANCE_0_7.md) distinguent centre prêt et file vide. La [revue lacustre Opus](LAKE_REVIEW.md) documente les domaines métriques, les régressions et le soulèvement artificiel des berges reconstruites.


Validation finale du code : **144 tests Rust**, **18 tests JavaScript**, `cargo fmt --all --check`, build natif release et build WASM release réussis. Les 71 tests du cœur comprennent les confluences de niveaux différents, le maintien d’un plan commun et la section isolée jusqu’à 99,5 % de sa largeur. Les avertissements de code mort du renderer WASM restent présents.

Contrôle WebGPU de la révision `spherical-coast-6` sur la 3090 : au raccord fluvial problématique, altitude 300 m, inclinaison réelle 25°, relief ×1, la coupe atteint L16 avec zéro fallback et aucune erreur de page. Les parois noires précédentes ne sont plus visibles : [avant](screenshots/confluence-before-0.7.png), [après](screenshots/confluence-after-0.7.png). La capture avant affichait encore 0° dans le panneau, mais la caméra mesurée était déjà à 25° ; le contrôle après synchronise aussi le panneau. Ces images valident ce voisinage, pas toutes les confluences de la planète.

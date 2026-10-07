# World Builder Rust

Prototype de planète sphérique : génération Rust locale, érosion globale CPU, érosion régionale CUDA ou CPU, exploration Rust/WebAssembly dans le navigateur. Projet indépendant des anciennes tentatives dans `world_builder`. Aucun entraînement n'est obligatoire.

## Démarrer

Installer [Rust avec rustup](https://rustup.rs), puis :

```bash
./scripts/run.sh
```

Ouvrir **http://localhost:8001** dans Chrome ou Edge récent. Le premier lancement compile le serveur et le WASM, installe `wasm-bindgen-cli` et prépare la graine 42. Les suivants réutilisent les binaires et le monde enregistré. Après une mise à jour du générateur, régénérer le monde depuis l'interface pour bénéficier du nouvel algorithme.

Sous Windows : `powershell -ExecutionPolicy Bypass -File scripts/run.ps1`. Sous WSL : lancer le script Linux et ouvrir l'URL dans le navigateur Windows. Le calcul régional peut utiliser CUDA dans WSL, tandis que le rendu utilise le GPU du navigateur Windows. Firefox préfère WebGL2 ; les autres navigateurs essaient WebGPU en premier lorsqu’un adaptateur est disponible. **http://localhost:8001/?backend=webgl** préfère explicitement WebGL2 puis tente WebGPU en cas d’échec.

Dans ce workspace, [le lanceur parent](../docs/world_builder_hybrid.md) démarre
également la passerelle InfiniteDiffusion sur8767. Le mode hybride est le
choix par défaut dans Génération : base des continents procédurale et hauteurs
NN locales complètes à 30 m, avec aperçus appris. Le bouton
**Voir montagnes NN · vue à 18 km** ouvre la vue colorée
avec hauteur, pente et apport NN mesurés. `./scripts/run.sh` seul ne lance
pas cette passerelle. Sous Firefox Windows/RDP, utiliser le profil dédié
du lanceur `C:\workspace\map-denoise-firefox.cmd`.

CUDA requiert le pilote NVIDIA et NVRTC, fourni par le CUDA Toolkit. Le script Linux ajoute `/usr/lib/wsl/lib` et `/usr/local/cuda/lib64` au chemin des bibliothèques. Sous Windows, rendre le dossier `bin` du Toolkit accessible dans `PATH`, par exemple avec `$env:Path = "$env:CUDA_PATH\bin;$env:Path"`. Sans CUDA/NVRTC, le serveur revient automatiquement au CPU ; le rapport régional indique le backend et la raison du repli. Le benchmark CUDA explicite échoue si CUDA est absent.

## Explorer et régler le monde

Le panneau Génération expose tous les paramètres : planète, relief, détail côtier, fjords polaires, densité des archipels, petits lacs et marais, érosion, océans et climat. Les préréglages et l’import/export JSON conservent la graine ; un indicateur distingue les réglages modifiés de ceux du monde chargé. Commencer avec un atlas 64 ou 128 par face. L'API accepte jusqu'à 1024, avec un coût CPU, RAM et stockage croissant. Le maillage d'aperçu reste plafonné à 128 par face, indépendamment de l'atlas physique.

Faire glisser pour tourner, utiliser la molette pour descendre ou « Aller à… » pour rejoindre un relief sans glace, une côte, un lac, une rivière ou une vallée fluviale. Incliner la vue jusqu’à 35° pour observer les pentes. Les saisons actualisent climat et biomes simplifiés ; les débits restent annuels. La couche rivières est activée par défaut et se règle dans l’onglet Vue. Villes et routes sont retirées de cette interface pour concentrer le prototype sur la planète. Le relief ×1 garde ses dimensions physiques.

Les réglages par défaut utilisent **8 étapes sur 350 000 ans**, un détail côtier de **30 %** et une échelle minimale de **0,25 m**. Augmenter la durée géologique augmente l’incision ; augmenter le nombre d’étapes augmente aussi le coût. Le préréglage « Essai rapide » utilise un atlas 64 et quatre étapes.

Relief et côtes combinent un atlas évolué par érosion et des bandes fractales adressées par position globale. L’exploration utilise `generator=coherent` : le globe orbital et toutes les tuiles évaluent le même champ, sans activation d’une nouvelle région érodée au zoom. Le raffinement révèle des sommets intermédiaires ; il ne change pas les sommets communs. Le générateur régional `spatial` reste accessible par API pour les expériences ultérieures. Le déplacement horizontal de la côte est calculé une fois par position, avec une échelle minimale configurable ; le détail vertical ne change pas son signe terre/mer. La protection du détail autour des lacs varie continûment, sans marche carrée au bord des cellules. Les lacs de l’atlas physique ont un niveau horizontal et un budget d’eau ; les dépressions peu profondes sont drainées géométriquement pendant la génération.

Les provinces glaciaires et les archipels ajoutent des objets fixes avant leur transport côtier : vallées ramifiées, chaînes d’îles de tailles variées, lacs et marais locaux. Le champ insulaire comprend sept tailles, des îlots aux grandes îles, avec des fondations pour les chaînes océaniques. Quatre populations lacustres comprennent les plans d’eau de quelques dizaines de mètres ; le zoom résout les objets existants. Les destinations « Fjords polaires », « Archipel et îlots », « Pays des lacs et marais » et les rivières sont vérifiées sur le champ rendu. Ces ajouts visuels ne recalculent pas le budget hydrologique de l’atlas ; les petits lacs n’ont pas encore d’exutoires simulés. Voir [les côtes et les îles](docs/COAST_0_10.md) et [les mesures lacustres](docs/QUALITY_REFERENCE.md).

Le mode « Biomes et eaux » mélange végétation, sol, roche et neige à six échelles fixes, de **64 km à 2 m**, filtrées selon leur taille en pixels. Les populations de biomes suivent un climat continu et leurs frontières deviennent des mosaïques fragmentées. Les couleurs tiennent compte de la pente physique, de la température et de l’humidité : canopée vert foncé et clairières vert clair en plaine, roche sur les versants abrupts. Les coordonnées du bruit conservent leur précision métrique pendant le déplacement et le raffinement. La mer suit sa bathymétrie. Voir [les biomes](docs/BIOMES_0_10.md) et [la validation du rendu](docs/RENDERING_0_10.md).

Le navigateur prépare les maillages dans un worker WASM, conserve les buffers GPU et répartit des groupes indépendants de transitions entre les images. Le raffinement atteint le niveau 24 ; les normales restent valides aux échelles centimétriques. Les parents utiles au retour de zoom restent en cache, et les paquets d’un ancien monde ou réglage sont immédiatement invalidés. Il réutilise les tuiles en mémoire et dans un cache persistant ; le champ de vision et une couronne de sécurité reçoivent les tuiles fines, tandis que des tuiles grossières ferment le reste du globe. Le préchargement prépare aussi les enfants de la prochaine frontière, avec 64 tuiles anticipées maximum et un créneau de chargement réservé aux demandes visibles. Les déplacements ne relancent pas le morph des tuiles communes.

La coupe est graduée **2:1**, y compris pendant les transitions, pour éviter qu'un bord fin ne soit soulevé sur un voisin trop grossier. Les niveaux 1–2 utilisent 64 segments, puis 32 segments avec un niveau de raffinement supplémentaire pour conserver l'espacement. La coupe complète est limitée à 1 024 feuilles et les caches du viewer à **768 Mio CPU et GPU** chacun ; le niveau livré et la limite éventuelle sont exposés dans `worldBuilderPerf()`. Seules les tuiles dans le frustum sont dessinées. Les regroupements restent animés lorsqu'ils sont visibles ; les groupes invisibles ou plus petits qu'un demi-pixel peuvent terminer immédiatement, avec revérification des bornes et protection de la hauteur caméra.

Le serveur réutilise les tuiles dans un cache RAM LRU de **256 Mio** et un cache disque vérifié de **1 Gio** ; les demandes identiques partagent leur calcul. Le worker du navigateur garde **128 Mio** en RAM et **384 Mio** sur disque, jusqu’à 2 048 paquets. `WB_TILE_CACHE_MIB=512 ./scripts/run.sh` augmente le cache RAM serveur. **http://localhost:8001/api/performance** et les en-têtes `Server-Timing` / `X-World-Tile-Cache` distinguent attente, calcul et réutilisation. Ces optimisations ne changent ni le relief ni l'identité du monde.

## Commandes utiles

```bash
cargo run --release -p world-app -- generate --config examples/planet.json
cargo run --release -p world-app -- preview --seed 123 --resolution 64
cargo run --release -p world-app -- validate
cargo run --release -p world-app -- serve --port 8001
cargo test --workspace
node --test web/*.test.mjs
cargo run --release -p world-core --example benchmark -- 128 1337
cargo run --release -p world-core --example generation_benchmark -- 512 2386084681
./scripts/build-web.sh
```

`generate` compile et sauvegarde ; `preview` choisit rapidement une graine ; `validate` vérifie le dernier atlas ; `serve` lance l'API et le visualiseur. `build-web.sh` recompile le WASM après une modification du renderer.

Comparer explicitement CUDA à la référence CPU sous WSL :

```bash
LD_LIBRARY_PATH="/usr/lib/wsl/lib:/usr/local/cuda/lib64:${LD_LIBRARY_PATH:-}" \
  cargo run --release -p world-erosion --example compare -- 257
```

Remplacer `257` par `1025` pour une grille plus grande. RTX 3090, solveur v2, 160 pas, après initialisation : **257² en 5,93 ms CUDA / 1 149 ms CPU** ; **1025² en 82,75 ms / 23 441 ms**. Le rapport donne différence CPU/CUDA, conductivité effective et bilans d'eau et de solides. Les conditions figurent dans [l'état d'implémentation](docs/IMPLEMENTATION.md).

## Organisation et données

- `world-core` : sphère, atlas, érosion globale, drainage, réservoirs, climat, biomes et échantillonnage continu.
- `world-erosion` : solveur régional de ruissellement, transport, érosion/dépôt ; CPU et CUDA/NVRTC.
- `world-app` : commandes, stockage vérifié, régions persistantes et API HTTP.
- `world-human` : localités agrariennes, population et routes sur l'atlas.
- `world-viewer` et `web` : renderer Rust/WASM, shaders, caméra et interface.
- `world-contracts` : subdivision géométrique certifiée expérimentale, accessible par CLI/API et masquée dans l'interface courante.

Les atlas, régions et checksums vivent dans `data/worlds/<id>/`. Les régions conservent aussi eau restante et sédiments suspendus. `data/tile-cache/` contient seulement des paquets recalculables. `--data <répertoire>` change le stockage. Le serveur écoute sur localhost ; `--host 0.0.0.0` autorise l'accès depuis le réseau local.

## Portée

La version **0.10** ajoute des matières multiechelles conditionnées par la pente, davantage de petites îles et de bassins locaux, des fjords courbes et une coupe LOD graduée. Le nouvel atlas utilisateur 512² × 6, graine `2386084681`, a été généré et sauvegardé en **27,46 s**. Les [mesures de rendu](docs/RENDERING_0_10.md) distinguent la cadence et les délais de chargement ; les [revues Opus et Sonnet 5.5](docs/LOD_REVIEW_0_10.md) documentent les garde-fous du regroupement de tuiles.

La version **0.8** réduit l’érosion par défaut et accélère la préparation. Pour la graine `2386084681`, atlas **512² × 6** : **20,12 s de génération**, **24,98 s avec validation et sauvegarde**, contre 103,76 s de génération avec les anciens paramètres. Le changement d’altitude RMS baisse de **980,54 à 284,84 m**. Ces gains combinent optimisations du calcul et durée géologique réduite ; la résolution de l’atlas reste identique. Les [mesures 0.8](docs/PERFORMANCE_0_8.md), la [revue côtière](docs/COAST_REVIEW.md) et le [rapport des rivières](docs/RIVER_REVIEW.md) donnent les contrôles et leurs limites.

La [comparaison avec Burgmap](docs/QUALITY_REFERENCE.md), la [validation de l’érosion 0.7](docs/EROSION_VALIDATION.md) et l’[audit Sonnet](docs/AUDIT_BUGS_PERFORMANCE.md) conservent les relevés antérieurs. La [revue des lacs](docs/LAKE_REVIEW.md) explique leurs contraintes de reconstruction. Le terrain local de `city_generator` évolue à environ 64 m ; notre atlas 512 a un espacement moyen de 4,91 km. Le maillage fin ne remplace pas encore cette simulation locale.

L'[architecture de recherche](docs/DESIGN_MONDE_MULTIECHELLE.md) dépasse les capacités actuelles. Les bilans sont mesurables, mais lois et coefficients restent simplifiés. L'érosion régionale simule une pluie finie ; son horloge n'est pas calibrée sur l'histoire géologique. Climat paramétrique, réservoirs sans hiérarchie Fill–Spill–Merge complète et absence de restriction conservatrice exacte entre niveaux restent des limites explicites. L’intégration InfiniteDiffusion fournit les hauteurs complètes des zones explorées via la passerelle externe ; les continents et l’hydrologie initiaux restent procéduraux. Le détail sous 30 m est interpolé, avec matériaux procéduraux, sans nouvelles structures apprises. Le checkpoint n’a pas de canal d’historique glaciaire.

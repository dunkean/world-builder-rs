# World Builder Rust

Prototype de planète sphérique : génération Rust locale, érosion globale CPU, érosion régionale CUDA ou CPU, exploration Rust/WebAssembly dans le navigateur. Projet indépendant des anciennes tentatives dans `world_builder`. Aucun entraînement n'est obligatoire.

## Démarrer

Installer [Rust avec rustup](https://rustup.rs), puis :

```bash
./scripts/run.sh
```

Ouvrir **http://localhost:8001** dans Chrome ou Edge récent. Le premier lancement compile le serveur et le WASM, installe `wasm-bindgen-cli` et prépare la graine 42. Les suivants réutilisent les binaires et le monde enregistré. Après une mise à jour du générateur, régénérer le monde depuis l'interface pour bénéficier du nouvel algorithme.

Sous Windows : `powershell -ExecutionPolicy Bypass -File scripts/run.ps1`. Sous WSL : lancer le script Linux et ouvrir l'URL dans le navigateur Windows. Le calcul régional peut utiliser CUDA dans WSL, tandis que le rendu utilise le GPU du navigateur Windows. WebGPU est prioritaire ; **http://localhost:8001/?backend=webgl** force WebGL2.

CUDA requiert le pilote NVIDIA et NVRTC, fourni par le CUDA Toolkit. Le script Linux ajoute `/usr/lib/wsl/lib` et `/usr/local/cuda/lib64` au chemin des bibliothèques. Sous Windows, rendre le dossier `bin` du Toolkit accessible dans `PATH`, par exemple avec `$env:Path = "$env:CUDA_PATH\bin;$env:Path"`. Sans CUDA/NVRTC, le serveur revient automatiquement au CPU ; le rapport régional indique le backend et la raison du repli. Le benchmark CUDA explicite échoue si CUDA est absent.

## Explorer et régler le monde

L'interface expose graine, résolution d'atlas, rayon, fraction océanique, gravité, obliquité, relief, pas d'érosion et durée géologique. Commencer avec un atlas 64 ou 128 par face. L'API accepte jusqu'à 1024, avec un coût CPU, RAM et stockage croissant. Le maillage d'aperçu reste plafonné à 128 par face, indépendamment de l'atlas physique.

Faire glisser pour tourner, utiliser la molette pour descendre ou « Aller à… » pour rejoindre un relief sans glace, une côte, un lac, une rivière ou une vallée fluviale. Incliner la vue jusqu’à 35° pour observer les pentes. Les saisons actualisent climat et biomes simplifiés ; les débits restent annuels. Rivières, villes et routes peuvent être masquées. Le relief ×1 garde ses dimensions physiques.

Relief et côtes combinent un atlas évolué par érosion et des bandes fractales adressées par position globale. Les lacs ont un niveau horizontal et un budget d'eau ; les chenaux suivent le drainage. Au zoom, les tuiles partagent des régions érodées et persistées aux niveaux 6 et 10. La région fine hérite du parent ; des bandes différentielles d'érosion s'ajoutent au relief analytique sans effacer ses fréquences plus fines. Leur moyenne locale est retirée pour éviter des cuvettes carrées aux frontières des régions. Le premier passage calcule les régions, les suivants les rechargent.

Le navigateur prépare les maillages dans un worker WASM, conserve les buffers GPU et répartit les transitions entre les images. Il réutilise les tuiles en mémoire et dans un cache persistant ; en vue rapprochée, seuls le champ de vision et une couronne de sécurité sont dessinés. Les déplacements ne relancent pas le morph des tuiles communes.

Les tuiles sont réutilisées dans un cache RAM LRU de **256 Mio** et un cache disque vérifié de **1 Gio** ; les demandes identiques partagent leur calcul. `WB_TILE_CACHE_MIB=512 ./scripts/run.sh` augmente le cache RAM. **http://localhost:8001/api/performance** et les en-têtes `Server-Timing` / `X-World-Tile-Cache` distinguent attente, calcul et réutilisation. Ces optimisations ne changent ni le relief ni l'identité du monde.

## Commandes utiles

```bash
cargo run --release -p world-app -- generate --config examples/planet.json
cargo run --release -p world-app -- preview --seed 123 --resolution 64
cargo run --release -p world-app -- validate
cargo run --release -p world-app -- serve --port 8001
cargo test --workspace
node --test web/tile-stream.test.mjs
cargo run --release -p world-core --example benchmark -- 128 1337
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

L'[architecture de recherche](docs/DESIGN_MONDE_MULTIECHELLE.md) dépasse les capacités actuelles. Les bilans sont mesurables, mais lois et coefficients restent simplifiés. L'érosion régionale simule une pluie finie ; son horloge n'est pas calibrée sur l'histoire géologique. Climat paramétrique, réservoirs sans hiérarchie Fill–Spill–Merge complète et absence de restriction conservatrice exacte entre niveaux restent des limites explicites. Le mode natif et le modèle appris ne sont pas implémentés.

# World Builder Rust

Prototype de monde procédural sphérique, généré en Rust et exploré avec un visualiseur Rust/WebAssembly. Projet indépendant des anciennes tentatives dans `world_builder`.

## Démarrer

Installer [Rust avec rustup](https://rustup.rs), puis :

```bash
./scripts/run.sh
```

Ouvrir **http://localhost:8001** dans Chrome ou Edge récent. Le premier lancement compile le serveur et le WASM, installe `wasm-bindgen-cli` et prépare la graine 42. Les lancements suivants réutilisent les binaires et le monde enregistré. WebGPU est prioritaire ; **http://localhost:8001/?backend=webgl** force le repli WebGL2. Le calcul global tourne côté serveur local, le rendu côté navigateur : aucun abonnement GPU ou entraînement n'est requis.

Sous Windows : `powershell -ExecutionPolicy Bypass -File scripts/run.ps1`. Sous WSL : lancer le script Linux et ouvrir l'URL dans le navigateur Windows. Le rendu utilise ainsi le GPU du navigateur sans dépendre de Vulkan dans WSL. Rust, les scripts et leurs dépendances sont requis localement.

## Utilisation

L'interface permet de changer graine, résolution, rayon, fraction océanique, gravité, obliquité et relief. Faire glisser pour tourner, utiliser la molette pour zoomer, ou choisir un sommet, une rivière ou une localité dans « Aller à… ». Les saisons actualisent le climat et les biomes sans régénérer le terrain ; les débits restent annuels. Rivières, villes et routes peuvent être masquées.

La case **Raffinement conservatif** active une descente dans les parents déjà raffinés, avec conservation des moments d'altitude et certificats à chaque niveau. Ce mode expérimental ne garantit pas encore le drainage ou les côtes. Le mode spatial par défaut privilégie le relief continu et les chenaux de l'atlas. Les mouvements de caméra ne changent pas l'identité du monde.

```bash
cargo run --release -p world-app -- generate --config examples/planet.json
cargo run --release -p world-app -- preview --seed 123 --resolution 64
cargo run --release -p world-app -- validate
cargo run --release -p world-app -- refine-demo
cargo run --release -p world-app -- serve --port 8001
cargo test --workspace
cargo run --release -p world-core --example benchmark -- 128 42
./scripts/build-web.sh             # recompiler le visualiseur
```

Les atlas et leurs checksums sont sauvegardés dans `data/worlds/<id>/`. Le serveur recharge le dernier monde au redémarrage. `--data <répertoire>` change l'emplacement. Le serveur écoute sur localhost ; `--host 0.0.0.0` permet un accès depuis le réseau local.

## Organisation

- `world-core` : sphère, atlas, relief, eau, drainage, climat et biomes simplifiés, échantillonnage et tuiles.
- `world-contracts` : expérience de subdivision conservatrice avec certificat numérique.
- `world-app` : commandes, persistance vérifiée et API HTTP ; une génération à la fois.
- `world-human` : localités agrariennes, allocation de population et routes sur l'atlas.
- `world-viewer` : renderer Rust/WASM, shaders WGSL et caméra.
- `web` : interface et chargement du WASM ; aucune génération de planète en JavaScript.

## Portée du prototype

L'[architecture complète](docs/DESIGN_MONDE_MULTIECHELLE.md) est l'objectif de recherche, **pas la description de toutes les capacités actuelles**. Le prototype privilégie un parcours génération → stockage → exploration. Le climat est paramétrique, les lacs sont des dépressions au niveau de débordement, et les modes de détail local restent des approximations. La descente conservatrice est intégrée au service et au visualiseur ; elle ne certifie pas encore le drainage de toutes les tuiles métriques.

Les backends de calcul CUDA/WGSL, le climat saisonnier conservatif, l'érosion physique complète et le mode natif restent à développer. Le modèle appris est facultatif. Aucun objectif de réalisme géologique n'est présenté comme déjà atteint. Voir [l'état d'implémentation](docs/IMPLEMENTATION.md) pour les mesures et limites constatées.

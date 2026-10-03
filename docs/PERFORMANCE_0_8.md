# Génération, côte et navigation du prototype 0.8

Mesures du 3 octobre 2026 sur la machine de développement : CPU/Rayon
16 threads sous WSL, Chrome Windows avec WebGPU/RTX 3090, fenêtre 1280×900.
Graine **2386084681**, rayon 1 737 km, atlas **512² × 6**. Le monde courant
est `79eda9537bcefc9fb77d`, surface `spherical-coast-7`.

## Préparation et érosion

| Configuration et stade | Génération seule |
| --- | ---: |
| Ancien calcul, 24 étapes sur 2 millions d'années | 103,76 s |
| Climat précalculé et parcours inutiles supprimés, mêmes paramètres | 63,08 s |
| Nouveaux paramètres, 8 étapes sur 350 000 ans | 24,72 s |
| Tableaux Priority-Flood compacts, mêmes nouveaux paramètres | 20,12 s |

La commande complète `worldctl generate` prend **24,98 s**, avec validations,
sérialisation JSON, checksum, écriture atomique et synchronisation disque.
Compilation Rust, démarrage du serveur, aperçu et catalogue fluvial sont
exclus de ce délai. Les statistiques et le bilan d'érosion des deux premières
mesures sont identiques ; le gain suivant combine coût réduit et évolution
géologique volontairement plus courte. La résolution reste 512.

Le climat conserve latitude, humidité et température de référence ; seule
la correction d'altitude est réappliquée à chaque étape. Le drainage utilise
des tableaux compacts sans changer l'ordre des égalités du tas. Une passe
marine suffit avant le soulèvement ; un second flood est omis si aucun bassin
n'a été excavé. Les tableaux transitoires ajoutent environ 30 Mio à 512.

| Effet physique | Ancien réglage | Nouveau réglage |
| --- | ---: | ---: |
| Changement d'altitude RMS | 980,54 m | 284,84 m |
| Substrat érodé | 3,52 × 10¹⁹ kg | 9,08 × 10¹⁸ kg |
| Bassins excavés, cumul des étapes | 86 947 | 4 940 |

Le changement RMS baisse de **71 %**. Le nouveau monde conserve 227 lacs,
9 324 cellules lacustres mouillées et 10 872 cellules fluviales. Aucun cycle
de drainage ; résidu relatif solide `1,31e-13`, hydrique sauvegardé `4,32e-10`.
Ces bilans concernent l'atlas, pas les corrections visuelles locales.

## Côte et rivières

Le champ côtier fixe utilise trois axes semés par bande, des fréquences
indépendantes et du bruit à points irréguliers. Ratio par défaut 0,30,
coupure 0,25 m ; les coefficients sont mis en cache par thread. La
[revue côtière](COAST_REVIEW.md) expose les propriétés et les limites.
La sonde retrouve 1 200 points parent/enfant identiques sur deux parcours
L0→L24, sans changement des points partagés dans les sept grilles côtières.

Le catalogue réutilise ses projections au lieu de les recalculer pour chaque
passe : **2,79 s** sur le nouveau monde pendant le QA, 3,06 Mio temporaires.
Les supports de vallée suivent la largeur du chenal ; les incisions profondes
retrouvent le terrain naturel sur une plus grande partie de la berge sèche.
Les rivières sont affichées par défaut. Leur couche affiche les surfaces
mouillées ; elle ne colore plus de larges bandes de terre avec le débit
interpolé de l'atlas. Le mode Drainage reste disponible pour l'inspection.
Le [rapport fluvial](RIVER_REVIEW.md)
publie les raccords contrôlés et les écarts restants du champ mélangé.

## Contrôle du navigateur

Caméra déplacée directement entre les vues ; attente jusqu'au niveau cible,
puis 750 ms avant capture. Le temps inclut cette attente et la capture.
Les caches étaient partiellement chauds. La file périphérique peut repartir
après l'observation : « centre prêt » ne signifie pas tout le globe détaillé.

| Vue | Niveau atteint | Temps observé | rAF p95 |
| --- | ---: | ---: | ---: |
| Orbite, 7 rayons | 2 | 2,73 s | 16,8 ms |
| Première descente, 1 000 km | 4 | 6,76 s | 16,8 ms |
| Côte, 500 m | 15 | 11,42 s | 16,8 ms |
| Même côte, 50 m | 19 | 12,22 s | 16,8 ms |
| Même côte, 5 m | 22 | 7,51 s | 16,8 ms |
| Jonction fluviale, 300 m, inclinaison 25° | 16 | 10,05 s | 17,0 ms |
| Grande rivière, 1 km, inclinaison 25° | 14 | 8,89 s | 16,9 ms |
| Retour en orbite | 2 | 3,00 s | 16,8 ms |

Aucune erreur de page, aucune Long Task >50 ms et aucun fallback de surface
sur ce parcours. Intervalle rAF maximal 22,6 ms. Les compteurs Rust atteignent
environ 805 Mo CPU / 543 Mo GPU ; ils excluent la réserve WASM, les paquets
worker et le pilote. Les grands sauts de caméra restent progressifs : les
maillages les plus fins n'arrivent pas immédiatement.

La capture initiale à 5 m a révélé un seuil de shader rejetant l'eau moins
profonde que 1 cm. Sur cette plage plate, il déplaçait visiblement le rivage.
Le shader découpe désormais à profondeur zéro. Après reconstruction WASM
et rechargement, 50 m/L19 et 5 m/L22 sont contrôlés de nouveau : mer présente
jusqu'au rivage, aucun fallback et aucune erreur de page. Ce deuxième essai
ne remesure pas les percentiles du parcours complet. Après suppression du
coloriage terrestre par débit, un dernier contrôle confirme encore la
jonction à 300 m/L16 et la côte à 5 m/L22, sans erreur ni fallback.

Captures : [côte à 50 m](screenshots/coast-0.8-50m.png),
[côte à 5 m](screenshots/coast-0.8-5m.png),
[rivière à 300 m](screenshots/river-0.8-300m.png).
Le matériau reste simple et les chenaux peuvent rester assez rectilignes
à une échelle inférieure à leur tronçon atlas. L'érosion régionale comparable
à `city_generator` n'est pas encore intégrée au champ cohérent.

## Reproduire

```sh
cargo run --release -p world-core --example generation_benchmark -- 512 2386084681
WB_PROFILE_GENERATION=1 cargo run --release -p world-core --example generation_benchmark -- 512 2386084681
cargo run --release -p world-app -- generate --seed 2386084681 --resolution 512
cargo run --release -p world-core --example quality_probe -- data/worlds/79eda9537bcefc9fb77d/atlas.json
cargo test --workspace
node --test web/*.test.mjs
./scripts/build-web.sh
```

Validation : **150 tests Rust**, **18 tests JavaScript**, formatage Rust,
compilation native release et WASM release. Les avertissements de code mort
du renderer restent présents. Ces contrôles ne certifient pas toutes les
graines, côtes ou confluences.

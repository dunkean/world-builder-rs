# Validation du rendu 0.10

Le rendu garde une géographie fixe : le zoom raffine l’échantillonnage,
les matières révèlent six bandes et la coupe conserve des voisins
séparés d’au plus un niveau. Voir [les biomes](BIOMES_0_10.md),
[la surface](SURFACE_0_10.md) et [les revues Opus/Sonnet](LOD_REVIEW_0_10.md).

**208 tests Rust et 20 tests JavaScript passent**, avec deux sondes
Rust ignorées par défaut ; formatage Rust et traduction WGSL/GLSL valides.

## Conditions et protocole

Contrôles du 4 octobre 2026 : Chrome Windows, WebGPU, RTX 3090
identifiée par CDP, pilote 32.0.16.1060 ; canvas 918 × 773.
Graine `2386084681`, atlas 512² × 6, rayon 1 737 km, relief ×1,
archipels 0,884 ; monde `8f797784bac1c2eb8363`, générateur
`prototype-0.10.0`, surface `spherical-coast-10`. Le serveur tournant
est le nouveau binaire, et la page recharge le WASM reconstruit.

```sh
cargo test --workspace
node --test web/*.test.mjs
cargo fmt --all -- --check
bash scripts/build-web.sh
bash scripts/qa-renderer-cli.sh
```

Le dernier script réutilise une seule page Chrome Windows avec la
session CLI `coherence` ; `WB_RENDERER_CLI` permet de remplacer le
wrapper local. Il nécessite l’atlas ci-dessus et le serveur sur 8001.
Il sonde les destinations certifiées et cherche les rivages par
bissection. Les sorties volumineuses restent dans
`output/playwright/renderer-10/`, ignoré par Git.

Le critère de disponibilité exige le niveau central demandé et aucune
préparation pending pendant 600 ms, sondage toutes les 200 ms, puis
trois secondes de cadence stabilisée. Certaines préparations concernent
le support périphérique : ce délai ne mesure pas le premier pixel fin
au centre. Les files de téléchargement sont rapportées séparément.
La caméra est limitée à **5 m** ; un ancien scénario nommé « 2 m »
était donc rendu à 5 m. Les captures PNG et les diagnostics géométriques
ne font pas partie de la fenêtre de cadence.

Les intervalles `requestAnimationFrame` mesurent la cadence observée,
pas des timestamps GPU. Les durées de soumission CPU ne mesurent pas
le travail GPU. Les budgets du viewer excluent les mémoires générales
du navigateur, du worker et du pilote.

## Matrice avant les regroupements invisibles instantanés

Cette matrice inclut le bruit de gradients et les voisins locaux,
mais précède le dernier chemin instantané. RAM cliente froide au
rechargement ; caches disque et serveur potentiellement chauds.
Les [résultats condensés](RENDERING_0_10_BEFORE_INSTANT.json) conservent
les compteurs, la cadence par phase et les octets mesurés.

| Scène | Disponibilité | rAF p95 stabilisée |
|---|---:|---:|
| Orbite maximale | 4,335 s | 16,8 ms |
| Molette + glisser + retour, parcours compris | 9,378 s | 16,8 ms |
| Première côte à 1 000 km | 6,594 s | 16,8 ms |
| Fjord large / rive 500 m / rive 50 m | 12,013 / 9,732 / 12,088 s | 16,8 ms |
| Archipel / rive 500 m | 7,780 / 9,899 s | 16,8 ms |
| Pays des lacs 8 km / 1 km | 21,368 / 8,212 s | 16,8 ms |
| Rivière large / 300 m | 19,798 / 5,832 s | 16,8 ms |
| Revisite rive 500 m | 21,528 s | 16,8 ms |
| Rives fjord / île à 5 m, inclinaison 30° | 11,104 / 22,915 s | 16,8 ms |
| Retour orbital | 27,735 s | 16,8 ms |

Les seize scènes ne présentent aucune erreur page/console/GPU, aucune
tâche principale de 50 ms ou plus, aucun `surface_fallback`, ni écart
de LOD supérieur à 1. Le p95 de transition vaut environ 16,8 ms sauf
pour la première côte à 1 000 km, où il atteint 33,3 ms. Le maximum
mesuré vaut 49,9 ms. Les pics comptabilisés sont **767,969 Mio CPU**
et **682,842 Mio GPU**, sous les deux budgets de 768 Mio.

La revisite n’est pas un test à cache chaud : les déplacements ont
évincé les paquets utiles ; elle recharge 752 tuiles réseau, avec un
seul hit disque et aucun hit RAM. Le retour orbital attend 155 commits
dont les animations de 0,16 s expliquent une grande partie du délai.
Cela motive le regroupement immédiat des zones invisibles ou ≤0,5 pixel,
avec protections caméra, contrôle des animations actives et revérification
des bornes juste avant commit.

## Revalidation du build final

Les seize scènes ont été rejouées après les garde-fous caméra/epoch,
la restauration des racines et le repère polaire. Les
[résultats finaux](RENDERING_0_10.json) donnent un p95 d’environ
**16,8 ms** dans chaque phase de chaque scène, zéro longue tâche,
zéro erreur et zéro fallback. Le plus grand intervalle rAF atteint
50,1 ms ; ce trou ponctuel ne correspond pas à une longue tâche
principale détectée. Les pics sont **767,906 Mio CPU / 682,644 Mio GPU**.
Chaque audit retrouve un écart de LOD maximal de 1.

| Scène | Disponibilité sur le build final |
|---|---:|
| Orbite / molette-glisser-retour / première côte | 5,153 / 9,577 / 5,641 s |
| Fjord large / rive 500 m / rive 50 m | 12,745 / 9,690 / 13,676 s |
| Archipel / rive 500 m | 8,553 / 10,106 s |
| Pays des lacs 8 km / 1 km | 19,772 / 8,194 s |
| Rivière large / 300 m | 18,585 / 7,093 s |
| Revisite rive 500 m | 21,116 s |
| Rives fjord / île à 5 m et 30° | 11,112 / 22,820 s |
| Retour orbital | **15,948 s** |

Le retour orbital effectue 154 commits, dont 65 immédiats, contre
155 commits tous animés dans la matrice précédente. Son délai baisse
de 27,735 à 15,948 s (environ 42 %). Les caches ne sont pas identiques
entre deux passages ; le compteur confirme l’utilisation du chemin
rapide, mais cette comparaison ne constitue pas un benchmark causal
isolant toutes les autres conditions. Les autres délais ne baissent
pas systématiquement : la géographie visible conserve son animation
et les destinations encore froides nécessitent toujours des calculs.

Captures du build final : [fjords](screenshots/0_10/fjords.png),
[îles](screenshots/0_10/archipelago.png),
[pays des lacs](screenshots/0_10/lakes.png),
[rivière](screenshots/0_10/river.png).

## Matériaux et sondes géométriques

Une forêt de fraction 0,951, humidité 0,585 et température 7,43 °C
est contrôlée à 1 km puis 125 m. Le remplacement du bruit de valeurs
par des gradients retire ses plateaux carrés et révèle des clairières
organiques sur une canopée sombre : captures à
[1 km](screenshots/0_10/forest-1km.png) et
[125 m](screenshots/0_10/forest-125m.png). Le terrain atteint environ 249 m ;
les pentes centrales de 4,45–4,69° conservent leur couverture végétale.

Sur la rive polaire à 5 m, 81 directions réparties sur un carré de
8 m donnent 243 lectures de sommets ([capture](screenshots/0_10/coast-5m.png)) : offsets de morph actuels et
finaux tous nuls après stabilisation, niveaux 21–22. Cela ne certifie
que cette zone ; les tests analytiques et la règle 2:1 couvrent le
mécanisme de raccord plus largement. Le diagnostic `visible` de
`surface_edge_audit` signifie « dans le frustum », sans occlusion
planétaire : une grosse bordure située derrière la planète peut
encore y figurer.

Le build final a aussi été contrôlé sur la forêt à 125 m : déplacement
de 5 m vers l’est, puis retour aux coordonnées initiales. Après
stabilisation de toutes les files, **709 614 pixels sont identiques**
([mesure](RENDERING_0_10_PHASE.json)),
zéro différence, sur tout le canvas 918×773. La première arrivée
froide demande 34,062 s ; le petit déplacement et le retour demandent
1,089 et 1,091 s, dont 800 ms de stabilité imposée par le contrôle.
Cela valide la phase dans cette scène à cache chaud ; les régressions
numériques contrôlent également les changements d’origine et l’éviction.

## Temps de préparation et limites

La préparation et sauvegarde du nouvel atlas utilisateur prend
**27,461 s**. La préparation froide du catalogue hydraulique avec
les nouvelles populations locales prend **3,96–4,16 s**, contre
environ 19 s avant partitionnement et gardes exactes ; ce sont des
mesures distinctes, pas un gain de FPS. Les mesures lacustres et
leurs comparaisons entre résolutions figurent dans
[QUALITY_REFERENCE.md](QUALITY_REFERENCE.md).

La matrice utilise réellement WebGPU ; Naga 27 valide aussi
le WGSL et sa traduction GLSL ES 3.00. Un [contrôle WebGL2 réel](RENDERING_0_10_WEBGL.json)
a ensuite rendu la forêt à 1 km sur ANGLE/NVIDIA/D3D11, sans erreur
ni fallback de surface et avec toutes les files terminées
([capture](screenshots/0_10/forest-webgl.png)). Le niveau livré est
L15 pour L16 demandé, réduit par le plafond de 1 024 feuilles.
Ce contrôle de compatibilité, disponible après 22,957 s, ne mesure
pas la cadence et ne remplace pas la matrice entière WebGPU. Les normales de pente
restent celles du maillage résolu. L’érosion locale détaillée et les
exutoires des petits bassins ne sont pas encore simulés dans le bilan
physique global. La cadence fluide ne signifie pas un raffinement
instantané pendant un déplacement rapide.

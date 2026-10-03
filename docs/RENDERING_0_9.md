# Rendu cartographique

Le mode « Biomes et eaux » utilise une palette sobre : prairies olive,
forêts profondes, déserts ocres, roches chaudes, marais et glace ivoire.
Les fractions de biomes calculées par le serveur restent mélangées ; le
visualiseur ne reclasse pas le terrain. Les frontières sont perturbées
dans le sampler en coordonnées mondiales `f64`, à 30 km, 3 km et 300 m.
La perturbation repondère les fractions terrestres présentes, puis les
renormalise ; elle conserve un biome pur et n'ajoute aucune fraction d'eau.
Les marais locaux proviennent du modèle humide, pas d'une peinture du shader.

## Eau et relief

La mer passe du turquoise littoral au bleu profond avec sa profondeur
réelle, avec une transition de plateau continental. Lacs et rivières
gardent des encres distinctes. Une légère variation colore les plages,
les berges et les faces rocheuses. Le relief reçoit un ombrage moins dur
pour rester lisible dans les versants ombragés.

Ces effets modifient le RGB uniquement : positions, plans hydrauliques,
profondeurs et marqueurs de morphing restent inchangés. Le shader conserve
le rejet de l'eau à profondeur `<= 0`, les rivières activées par leur calque
et une sortie opaque (`alpha = 1`). La palette permet de distinguer mer
et eau douce sans ajouter un attribut ou changer les marqueurs du LOD.

## Grain fixe à plusieurs échelles

Le shader applique un bruit 3D de valeur à interpolation quintique,
ancré à la direction planétaire et à la graine entière du monde. Ses
longueurs sont 8 km, 400 m et 20 m. Deux mots `u32` conservent les 64 bits
de graine ; ni l'écran, ni le temps, ni la caméra, ni la clé de tuile
ne choisissent la phase du bruit.

Les dérivées du pixel filtrent les fréquences sous-échantillonnées, dont
les calculs sont sautés. Le grain de 20 m convient à la précision `f32`
de la direction pour le rayon de référence de 1 737 km ; ce rendu ne
prétend pas fournir une texture au centimètre. Les champs diagnostics
altitude, température, humidité et drainage gardent leurs rampes sans grain.

## Validation et coût

```sh
cargo test -p world-viewer --lib
bash scripts/build-web.sh
```

47 tests viewer passent, dont conservation des mélanges, niveaux de rive,
profondeurs millimétriques, marqueurs fluviaux et graines importées.
Le shader a aussi été validé avec Naga 27 et traduit en GLSL ES 3.00
pour WebGL2. Le build WASM passe. Aucun sommet, buffer de tuile ou cache
supplémentaire n'est ajouté ; l'uniforme de graine ajoute 16 octets.
Le coût maximal du grain est de trois octaves, soit 24 coins de bruit
par fragment en mode carte. Les durées CPU de préparation et de soumission
ne mesurent ni le temps GPU ni ses FPS. Captures et cadence doivent être
contrôlées sur le navigateur réel, après reconstruction du serveur et
chargement des nouveaux lieux ; une durée GPU exige un chronométrage GPU.

## Contrôle navigateur reproductible

`bash scripts/qa-renderer-cli.sh` réutilise la session Chrome Windows
`coherence` via `/tmp/wb-win-cli.sh` et le backend 0.9 sur le port 8001.
Il recharge cette seule page, récupère les lieux certifiés de l'atlas
`d8d1261ca22a1815f74f`, et cherche des rives marines par sondes/bissection.
Le cache RAM repart vide ; caches persistants et serveur peuvent être chauds.

Le passage molette/glisser contrôle les premières transitions depuis
l'orbite et le retour ; les autres scènes visitent fjord, îles, petits lacs
et rivière par `fly_to`, avec capture et cadence après stabilisation.
Les PNG sont exclus des fenêtres de mesure. Les résultats et captures
arrivent dans `output/playwright/renderer-09/`. Le critère de disponibilité
porte sur le niveau central ; les files de préchargement sont rapportées
séparément. Cette suite ne prouve pas tous les points de la planète.

## Mesures avant le correctif de préchargement — 3 octobre 2026

Chrome Windows, WebGPU, adaptateur `nvidia / ampere` ; CDP identifie la
GeForce RTX 3090. Atlas 512, graine `2386084681`, générateur 0.9,
surface `spherical-coast-9`, canvas 918 × 773. Le premier essai avait
révélé une syntaxe acceptée par Naga mais refusée par Chrome/Tint : les
produits du hash WGSL ont été parenthésés avant de relancer la suite.
Les chiffres suivants proviennent uniquement du rendu corrigé.

| Scène | Premier critère de disponibilité | rAF p95 stabilisée |
|---|---:|---:|
| Orbite maximale | 3,41 s | 16,8 ms |
| Molette + glisser + retour | 8,11 s, parcours compris | 16,8 ms |
| Premières côtes, 1 000 km | 4,64 s | 16,8 ms |
| Fjord, 22 km / rive 500 m / rive 50 m | 9,32 / 7,78 / 9,08 s | 16,8 ms |
| Archipel / rive 500 m | 13,76 / 9,27 s | 16,8 ms |
| Pays des lacs, 8 km / 1 km | 8,21 / 10,52 s | 16,8 ms |
| Rivière, 3,38 km / 300 m | 14,08 / 8,18 s | 16,8 / 16,9 ms |
| Revisite rive 500 m / retour orbite | 7,78 / 12,31 s | 16,8 ms |

Le critère exige le niveau central demandé et une file de préparation
vide pendant 600 ms ; sondage toutes les 200 ms, puis trois secondes de
cadence stabilisée. Dans trois scènes, de nouvelles préparations étaient
en cours à la capture, avec le niveau central toujours correct. Cela ne
prouve pas que tout le frustum ou le préchargement était terminé.

Les 14 scènes n'ont produit aucune erreur console/page/GPU, aucune tâche
principale de 50 ms ou plus, et aucun `surface_fallback`. Le plus grand
intervalle rAF mesuré était 24 ms. Pics comptabilisés par le cache du
viewer : 767,29 Mio CPU et 536,25 Mio GPU ; ce ne sont pas les mémoires
totales du navigateur ou du pilote. Le viewport de la rive à 500 m est
identique au pixel après visite des autres régions ; l'orbite initiale et
finale aussi : 709 614 pixels comparés dans chaque paire, zéro différence.

Mer, branches du fjord, îles, petits lacs et chenal fluvial sont visibles
et opaques dans les captures stabilisées, sans carré d'eau observé dans
ces vues. La rivière certifiée a 2,393 m de profondeur au point visité.
La carte garde deux limites importantes : les premières tuiles affichées
pendant le zoom rapide restent grossières (L5 pour une demande L10),
fermant temporairement des bras marins en polygones ; le détail du relief
présente encore des stries directionnelles au fort zoom. La cadence est
fluide, mais le raffinement n'est donc pas instantané ni visuellement
parfait pendant un scroll rapide. La revisite montre aussi un réemploi
insuffisant : elle a déclenché 224 requêtes réseau et seulement trois
hits disque côté worker. Le rendu final est stable ; disponibilité et
qualité du champ de relief restent à améliorer.

Rapport brut : `output/playwright/renderer-09/report.json`. Captures utiles :
`renderer-09-wheel-coast-fast.png`, `renderer-09-fjord-wide.png`,
`renderer-09-fjord-shore-50m.png`, `renderer-09-lake-province-8km.png`,
`renderer-09-river-wide.png` et `renderer-09-river-300m.png`, dans ce dossier.

## Revalidation après le correctif de préchargement

La [revue réelle Opus 5.5 du streaming](REVIEWS/OPUS_STREAMING_0_9_2026-10-03.md)
est un audit de code. Le correctif retenu projette un étage demandé dans
une coupe virtuelle et distingue requêtes essentielles et futures,
avec priorité au terrain visible ; il conserve géométrie et palette.

Une suite courte de quatre scènes a été exécutée sur la même machine,
après rechargement de la page : RAM client froide, caches disque/serveur
déjà chauds. Délai au premier critère de disponibilité : orbite 3,15 s,
molette/glisser/retour 7,47 s, fjord 22 km 8,52 s, rive 500 m 11,79 s.
Ces chiffres ne prouvent pas un gain causal du correctif : état des caches
et parcours diffèrent de la première suite. La rive a obtenu 220 hits
disque worker et cinq requêtes réseau, mais son délai dépassait celui du
premier passage (7,78 s).

Les 2 529 intervalles rAF donnent un p95 de 16,8 ms dans les quatre vues,
un maximum de 33,3 ms, zéro longtask, erreur ou `surface_fallback`.
Pics cache viewer : 767,09 Mio CPU et 495,57 Mio GPU. Les snapshots montrent
des extras Rust distincts, bornés à 64, et 0–44 requêtes JS prédites.
La capture finale à 500 m reste strictement identique à la baseline :
zéro pixel différent sur 709 614. Le retard visible pendant le scroll
rapide reste toutefois présent : au dixième cran, L5 affiché pour L10
demandé, comme avant. Le correctif répare le préchargement sans résoudre
à lui seul cette latence ni rendre ce parcours visuellement parfait.

Mesures/captures : `output/playwright/renderer-09-lookahead/`, référence
conservée dans `output/playwright/renderer-09-baseline/`.

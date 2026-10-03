# Profils fluviaux et raccords aux réservoirs

Le catalogue hydraulique est la référence commune de l'incision et de l'affichage.
Ses courbes, niveaux et sections sont calculés une fois par monde, sans dépendre
de la caméra, du LOD ou de l'ordre des requêtes.

## Contrôles canoniques

Les nœuds et les tangentes sont transportés par l'inverse de la déformation
côtière : le terrain et les rivières utilisent ainsi les mêmes coordonnées.
Chaque tronçon échantillonne le terrain naturel sur 128 segments fixes. Un
minimum cumulatif et une propagation sur le graphe de drainage produisent un
profil décroissant ; son interpolation Hermite monotone conserve une pente
continue. Les jonctions partagent exactement niveaux, largeurs et profondeurs.
Les cycles invalides sont exclus du catalogue visuel.

Le niveau marin reste fixé à zéro. Pour un lac ouvert, le contrôle et son
exutoire sec extérieur partagent le plan du réservoir. Un affluent traversant
un creux ne peut plus abaisser ce déversement. Le mélange continu du détail
vertical protège aussi ces contrôles secs. Les sorties réellement alimentées
restent présentes même sous le seuil ordinaire de 120 m³/s ; aucun exutoire
n'est inventé pour un bassin fermé.

Une entrée s'arrête à la première rive aval. Une sortie commence à la dernière
rive ascendante avant son contrôle, pour éviter de prendre un îlot pour une
sortie. Les recherches respectent toujours le domaine de courbe déjà découpé.

## Raccord sur toute la largeur mouillée

Un contact latéral avec un lac peut précéder l'arrivée de la ligne centrale.
Le profil rejoint donc progressivement un plateau au niveau du réservoir.
Avec `L` la longueur, `w` la largeur aval, `d` la profondeur aval et
`S = max(0, H_amont − H_aval) / L`, la longueur de remous vaut :

```text
min(0.8 L, max(3 w, 2 d / max(S, 10⁻⁶)))
```

Le raccord reste monotone et conserve le nœud amont partagé. Cette échelle
dimensionnelle est une heuristique de prototype, pas une résolution des
équations hydrodynamiques. Le plafond de 80 % peut manquer un contact latéral
exceptionnellement précoce sur une autre graine.

## Couplage des vallées confluentes

Partager le nœud final ne suffit pas : deux affluents peuvent se chevaucher
avant leur jonction, avec des profils séparés de plusieurs mètres. Trois
passes synchrones ajustent chaque profil sous les surfaces des autres vallées,
au centre et aux deux bords mouillés de ses 127 nœuds intérieurs. Le propre
tronçon est exclu ; dans un autre chenal mouillé, le plafond est sa surface
libre, jamais son lit. Cela évite d'approfondir réciproquement les profils à
chaque passe. Le minimum cumulatif restaure la monotonie sans déplacer les
nœuds partagés ou les contrôles mer/lac. Les passes natives sont parallèles ;
chaque passe lit le catalogue précédent immuable.

Le sampler commence par le minimum continu des vallées, puis mélange les lits
et les potentiels hydrauliques avec les mêmes poids : support égal à un dans
le chenal, décroissance C1 sur les berges. La profondeur quartique s'annule
avec une pente nulle à la rive. Un potentiel signé sur les berges sèches
empêche le mélange de créer une bosse lorsque deux sections ont le même plan.
Les candidats sont uniques aux coutures du cube. Le propriétaire ne fixe plus
une nouvelle hauteur ou un plan d'eau : la priorité binaire précédente
produisait des marches de 19,58 m au bord d'un affluent. Les plans lacustres et
marins restent prioritaires. Ce mélange est une reconstruction géométrique
bornée, pas un solveur hydraulique de confluence. Le couplage des profils est
compilé une fois par monde, pas à chaque tuile ou déplacement.

## Validation de cette révision

Atlas `32ede09abc580c1b3049`, graine **2386084681**, résolution **512² par face**,
coupure côtière de 1 m ; mesures du programme CPU `quality_probe` :

| Contrôle | Résultat |
| --- | ---: |
| Tests `world-core`, dont 19 géométriques | 71 réussis |
| Tronçons ; catalogue natif froid, pendant des builds concurrents | 10 135 ; 9,87 s |
| Centres de sections sur 512 grands tronçons | 15 872 |
| Profondeur fluviale maximale / profondeur canonique | 2,6701 m / 2,6701 m |
| Niveau maximal au-dessus du terrain naturel non incisé | 0,9312 m |
| Frontières humides de propriétaires localisées à moins de 1 mm | 23 |
| Sauts de surface supérieurs à 10 cm près des lacs ; maximum | 0 ; 0 à la précision f32 |
| Voisinages des dix anciens désaccords ; positions examinées | 10 ; 28 980 |
| Saut maximal de sol / de surface d'eau dans ces voisinages | 0,061 mm / 0 à la précision f32 |
| Profondeur locale maximale | 2,1408 m |
| Points communs parent/enfant, deux parcours L0→L24 | 1 200, aucun écart |

Le scan aquatique examine 500 100 positions près des lacs. Le relevé initial
de dix désaccords, dont un écart de profil de 14,85 m, a ensuite révélé de
vraies marches de sol et d'eau. La sonde conserve ces dix voisinages initiaux
même après correction. Elle localise 620 frontières, dont 106 paires mouillées
de propriétaires différents, et ne relève plus de saut supérieur à 10 cm.
Ces mesures concernent un monde et ne garantissent pas toutes les graines ou
tous les chevauchements. La profondeur du mélange est bornée par le maximum
des contributeurs ; le propriétaire choisi n'en définit pas le budget.
Avant le dernier mélange, le catalogue a aussi été mesuré
à 5,43 s ; ces durées froides ne mesurent pas le rendu d'une tuile en cache.

Le mélange laisse 57 positions où l'eau reconstruite diffère de plus de 2 cm
du profil canonique du même propriétaire, avec un écart maximal de 3,306 m.
Il ne s'agit plus d'une marche au changement de propriétaire : la surface
composite n'est pas partout identique au profil longitudinal isolé. Cela reste
une limite de l'approximation aux contacts avec les berges voisines ; le
catalogue conserve ses contrôles nodaux, mais son niveau isolé ne certifie pas
le niveau de chaque point de la surface mélangée.

## Revue indépendante et reproduction

La [revue CLI Opus 5.5](REVIEWS/OPUS_RIVER_PROFILE_2026-10-03.md) a réellement
inspecté une version intermédiaire. Elle a identifié deux erreurs de domaine
paramétrique lors du découpage des embouchures et des sorties ; elles sont
corrigées et couvertes par des régressions. Elle n'a pas exécuté les tests
ni revu le dernier ajustement du remous. La tolérance géométrique de 2 mm
signalée dans son troisième point reste explicite.

Une [seconde revue CLI Opus 5.5](REVIEWS/OPUS_CONFLUENCE_2026-10-03.md) a
identifié la priorité binaire et l'absence de couplage des profils incisés.
Les plafonds spatiaux ci-dessus suivent sa proposition ; leur validation
vient des régressions et des sondes exécutées après modification, pas de la
revue seule. Un plafond sous le niveau final d'un tronçon reste borné par
ce nœud : résoudre un croisement avec une vallée étrangère plus basse
nécessiterait aussi de revoir la connectivité hydraulique.

```sh
cargo test -p world-core
cargo run --release -p world-core --example quality_probe -- data/worlds/32ede09abc580c1b3049/atlas.json --overlap-reference=examples/overlap-regression-2386084681.json
```

La [fixture des dix voisinages](../examples/overlap-regression-2386084681.json)
conserve leurs coordonnées et la configuration ; ses indices ne s'appliquent
qu'à cet atlas 512². La validation finale comprend 144 tests Rust et 18 tests
JavaScript, les compilations native et WASM, et un contrôle du navigateur sur
RTX 3090. Les parois observées aux confluences ciblées disparaissent :
[avant](screenshots/confluence-before-0.7.png),
[après](screenshots/confluence-after-0.7.png).

La [référence de qualité locale](QUALITY_REFERENCE.md) détaille les contrôles
et l'écart restant avec une évolution de paysage régionale à l'échelle de
10 × 10 km. Les mesures CPU ci-dessus ne mesurent ni le GPU ni le rendu Web.

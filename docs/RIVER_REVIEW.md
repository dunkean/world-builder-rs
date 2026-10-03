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

## Validation historique de la révision 0.7

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

## Catalogue et incision de la révision 0.8

Les trois passes réutilisent désormais les projections courbes et le terrain
fixe, préparés une seule fois. Les nœuds déjà au niveau aval et les interactions
incapables de les abaisser sont exclus avant les calculs coûteux. La préparation
de l'ancien atlas 512 conserve 55 120 interactions dans 3,00 Mio temporaires,
libérés après compilation. Le catalogue a été mesuré à 1,41 s sur cet atlas,
contre 5,43 s pour les passes antérieures et 9,87 s pendant des builds concurrents.
Les profils, leurs nœuds et les contrôles restent partagés.

Le support d'une vallée suit maintenant la largeur du chenal :
`max(80 m, 6 × demi-largeur + 30 m)`, limité à 0,35 fois le pas atlas,
avec un minimum de `5 × demi-largeur + 20 m` pour conserver la section.
La grille grossière n'impose plus à chaque petit cours d'eau une vallée
de plusieurs kilomètres. Les vallées peu incisées gardent leur plaine
alluviale ; lorsque le terrain reconstruit est beaucoup plus haut que le
profil, le versant retrouve progressivement le terrain naturel sur une
plus grande partie du support sec. Le chenal mouillé reste exact. Cette
adaptation évite les terrasses profondes et leurs parois à la sortie du fondu.

Le test de continuité des berges conserve sa borne de 1 m sur 20 cm ; les
19 régressions de profil et d'embouchure passent aussi. La réduction du
support possède une régression sur un petit cours d'eau dans un atlas
très grossier. Les mesures de la section précédente restent historiques :
les nouvelles torsions côtières et les nouveaux paramètres d'érosion
nécessitent un atlas 0.8 pour évaluer la génération par défaut.

Le nouvel atlas **`79eda9537bcefc9fb77d`**, graine 2386084681, utilise 512²
cellules par face, 8 étapes d'érosion sur 350 000 ans et une coupure côtière
de 0,25 m. La génération `World::generate` est mesurée à environ 20,12 s ;
la commande complète `worldctl`, incluant validation, sérialisation,
écriture atomique et checksum, prend 24,98 s. Le catalogue contient
9 847 tronçons et prépare 56 536 projections dans 3,06 Mio temporaires ;
sa compilation froide prend 2,79 s pendant la validation navigateur, avec
d'autres tâches concurrentes.

| Contrôle du nouvel atlas 0.8 | Résultat |
| --- | ---: |
| Centres sur les 512 grands tronçons | 15 872 |
| Profondeur fluviale maximale | 2,3930 m |
| Profil à plus de 10 m au-dessus du terrain naturel | 0 |
| Écart positif maximal profil/terrain naturel | 0,8376 m |
| Voisinages avec différence catalogue/champ mélangé | 20 |
| Positions / interfaces localisées dans ces voisinages | 57 960 / 996 |
| Maximum sol / eau sur les interfaces | 0,610 mm / 0,122 mm |
| Sauts supérieurs à 10 cm | 0 |
| Interfaces près des lacs / paires mouillées | 12 067 / 3 |
| Différence d'eau aux interfaces lacustres | 0 à la précision f32 |

Les 20 différences catalogue/champ mélangé sont au même propriétaire ;
leur maximum baisse à 0,523 m. Elles restent une limite de la reconstruction
géométrique, sans prouver la monotonie du champ composite. Les 1 200 points
partagés des deux parcours L0→L24 restent identiques. Ces relevés concernent
ce monde, pas tous les terrains ou toutes les graines.

## Destinations d'exploration et visibilité

Les anciens raccourcis utilisaient le maximum de débit de l'aperçu ou le
début d'une polyligne, avec l'altitude générale de 30 km. Ils pouvaient viser
un nœud sec, un lac ou une section trop petite pour le maillage visible.
`/rivers.destinations` fournit maintenant des points intérieurs vérifiés par
`World.sample` : propriétaire fluvial exact, hors mer et lac, profondeur
d'au moins 10 cm. L'altitude conseillée vaut 18 largeurs réelles, bornée
entre 600 et 5 000 m. La vallée choisit une pente modérée plutôt que le
plus grand dénivelé. Aucun débit ni chenal n'est agrandi pour l'affichage.

La sélection active aussi le calque des rivières, qui commande la visibilité
de leur eau locale. Une altitude modifiée ensuite par l'utilisateur reste
respectée. La recherche CPU des destinations et des segments ne bloque plus
le worker asynchrone de l'API. La régression vérifie qu'un maximum sec ne
devient pas une destination et que chaque cible est à l'intérieur de son
chenal, à l'altitude adaptée à sa largeur.

Sur l'atlas 0.8 ci-dessus, avant ajout des nouveaux districts géographiques,
la Grande rivière sélectionnée mesure 188,018 m de large et 2,393 m de
profondeur ; la visite conseille 3 384 m. La vallée proposée mesure
187,459 m de large et 2,389 m de profondeur, à 3 374 m d'altitude de visite.
Les deux points sont mouillés avec `river = 1`, hors mer et lac. Les
20 tests géométriques et `cargo check -p world-app` passent. Ces contrôles
numériques ne remplacent pas le contrôle du rendu après compilation WASM.

Les courbes actuelles lissent le graphe atlas, dont les récepteurs suivent
huit directions voisines à plusieurs kilomètres d'intervalle. Cette base
explique encore des tronçons assez rectilignes. Des méandres locaux exigent
une courbe commune à l'incision, au profil et au rendu ; la réparation des
destinations ne modifie pas cette géométrie.

## Certification après ajout des districts 0.9

L'atlas **`d8d1261ca22a1815f74f`**, graine 2386084681, contient les nouveaux
champs fixes de fjords, d'archipels et de microbassins. Les deux destinations
fluviales ci-dessus restent exactement mouillées avec les mêmes profondeurs
après leur intégration. Le relevé initial 0.9 de 15 364 centres effectivement fluviaux,
sur 512 grands tronçons, trouve une profondeur maximale de 2,393 m et aucun
cas au-dessus de 10 m ; les points marins ou lacustres sont exclus.

`/landforms` certifie aussi ses lieux de visite. Un fjord doit être marin ;
si son point proposé est sec, la recherche essaie les segments intérieurs
du chenal. L'archipel cadre jusqu'à trois îles réellement émergées proches
du centre de la chaîne. Le pays des lacs vise un microbassin effectivement
mouillé, identifié dans la plage réservée `[2⁴⁰, 2⁴¹[`, au lieu d'un centre
de province éventuellement sec. Le sondage s'arrête au premier bassin valide,
avec au plus huit districts par type et 288 points locaux supplémentaires
par district lacustre. Aucun scan dense de la planète n'est nécessaire.

| Visite certifiée sur cet atlas | Résultat |
| --- | ---: |
| Fjord : lit / niveau marin ; altitude de visite | −53,756 m / 0 m ; 22 km |
| Archipel : altitude cadrant trois îles | 81,761 km |
| Microbassin recertifié après protection des réservoirs : profondeur ; altitude de visite | 0,900 m ; 8 km |
| Découverte géographique froide, catalogue hydraulique compris | 4,95–6,01 s |
| Même découverte en cache | 2,34–2,79 ms |
| Recherche fluviale après compilation du catalogue | 2,13–2,86 ms |

Les durées proviennent de deux exécutions avec d'autres validations
concurrentes ; la dernière découverte froide, après protection des réservoirs,
prend 4,951 s. Les directions suivantes sont celles de ce dernier contrôle :

| Destination | Direction cartésienne normalisée |
| --- | --- |
| Fjord | `[0.659379813, -0.745814634, -0.094756501]` |
| Archipel | `[-0.380366336, 0.665703614, 0.641997001]` |
| Pays des lacs | `[0.413550226, -0.791462030, -0.450071178]` |
| Grande rivière | `[-0.014522241, -0.749026368, 0.662381011]` |
| Vallée fluviale | `[-0.003027440, -0.748985546, 0.662579419]` |

L'API recertifie les destinations sur le champ courant ; ces coordonnées
constituent un relevé ponctuel et peuvent évoluer si les contraintes des
microbassins changent. Trois nouvelles régressions couvrent le repli du fjord,
le cadrage de l'archipel et la présence effective d'un microbassin :
huit tests géographiques et vingt tests fluviaux passent. Le scan initial 0.9,
avant les dernières protections des réservoirs, examine 4 802 points autour
du fjord et du microbassin. Il n'a localisé aucun contact
mouillé entre propriétaires distincts ; il n'a donc mesuré aucune marche
de plan d'eau à ces endroits. Cela ne certifie pas les contacts de tous
les districts, ni toutes les graines ou le rendu GPU.

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
cargo run --release -p world-core --example quality_probe -- data/worlds/79eda9537bcefc9fb77d/atlas.json
```

La [fixture des dix voisinages](../examples/overlap-regression-2386084681.json)
conserve leurs coordonnées et la configuration ; ses indices ne s'appliquent
qu'à l'ancien atlas 0.7. Pour rejouer ce diagnostic historique, utiliser
son fichier `32ede09abc580c1b3049/atlas.json` et ajouter
`--overlap-reference=examples/overlap-regression-2386084681.json`.
La validation 0.7 comprenait 144 tests Rust et 18 tests
JavaScript, les compilations native et WASM, et un contrôle du navigateur sur
RTX 3090. Les parois observées aux confluences ciblées disparaissent :
[avant](screenshots/confluence-before-0.7.png),
[après](screenshots/confluence-after-0.7.png).

La [référence de qualité locale](QUALITY_REFERENCE.md) détaille les contrôles
et l'écart restant avec une évolution de paysage régionale à l'échelle de
10 × 10 km. Les mesures CPU ci-dessus ne mesurent ni le GPU ni le rendu Web.

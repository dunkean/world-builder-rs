# Qualité locale et référence Burgmap

La référence demandée est le générateur TypeScript actif de
`D:\Workspace\Self\RPG\city_generator\web\src\gen\terrain`, lu sans modification.
Les fichiers Python historiques ne décrivent pas le générateur actuellement utilisé.
Ce document distingue les mécanismes effectivement présents, les mesures CPU et
les éléments encore nécessaires pour atteindre cette qualité sur une région de 10 × 10 km.

## Pourquoi le relief de référence fonctionne

`heightfield.ts` appelle `mountainRelief` dans `erosion.ts` pour les montagnes,
collines et vallées. Le relief vient d'une évolution de paysage : soulèvement
spatial, drainage par priority flood, incision implicite dépendant de l'aire
amont, transport thermique sur les pentes et diffusion. L'évolution compte
18 à 22 itérations sur une grille de 176 à 224 cellules de côté. Une marge de
22 % permet au drainage de dépasser la carte. Le bruit final décore surtout
les versants ; il ne remplace pas les vallées et leurs lignes de partage des eaux.

Sur 10 km, la référence produit une grille finale de 768², soit environ
13 m par cellule ; la grille d'évolution étendue de 224² a un pas de 64 m.
L'atlas planétaire 512² par face du prototype, pour un rayon de 1 737 km,
a un pas moyen équivalent de 4 910 m. Raffiner sa triangulation ne crée donc
pas automatiquement le réseau de petites vallées présent dans Burgmap.

`hydrology.ts` traite ensuite les rivières comme un système cohérent : profil
longitudinal lissé puis décroissant, incision bornée, chenal, berges, plaine
alluviale et versants. Les méandres ont une longueur d'onde liée à la largeur,
une amplitude réduite par la pente et une transition douce aux extrémités.
Le drainage est recalculé après incision. Ces contraintes comptent davantage
que l'ajout de détails aléatoires à une polyligne.

## Ce qui est appliqué au prototype

- L'érosion globale concentre l'incision sur les axes alimentés par l'amont,
  emploie le niveau marin comme base d'érosion et transporte les sédiments
  sur une longueur finie. Les faibles dépressions sont réellement drainées.
- La côte est une composition fixe de torsions sphériques inversibles,
  avec une décroissance fractale d'exposant 0,85 et une coupure par défaut
  à 1 m. Les contrôles hydrologiques sont transportés par la carte inverse.
  Le détail vertical conserve son signe autour du niveau marin. Ni la caméra,
  ni l'ordre des requêtes, ni le niveau de tuile ne choisissent une nouvelle graine.
- La protection du détail près d'un lac dépend continûment des poids de
  l'interpolation ; sélectionner brutalement un lac à la frontière d'un quad
  produisait auparavant des falaises carrées.
- Les réservoirs utilisent le même domaine métrique pour leurs plans d'eau,
  leurs franges sèches et la reconstruction de leurs lignes de partage.
  Les deux propriétaires d'une séparation sont choisis par rayon normalisé ;
  une troisième paire de lacs ne peut pas créer de crête dans leur intérieur.
  Un cache immuable de labels occupe 6 Mio à 512² par face et se partage entre
  clones. Le chemin d'échantillonnage ne consulte que le stencil local,
  sans recherche dans toutes les cellules de tous les bassins.
- Le même mélange continu borne le détail aux vrais contrôles de
  déversement et exutoires secs. Un nœud situé exactement au plan de
  déversement ne peut plus être abaissé par le bruit fin.
- Le catalogue `RiverReach` partage les positions, niveaux, largeurs et
  embouchures entre l'incision locale et la représentation des rivières.
  Les sections utilisent un chenal, des berges, une plaine alluviale et
  des versants raccordés, plutôt qu'une seule parabole plafonnée.

Les patches d'érosion régionale expérimentaux déjà présents restent distincts
du chemin de rendu cohérent utilisé par défaut. Leur présence dans le dépôt
ne signifie pas que toute région explorée bénéficie d'une simulation locale.
Les corrections lacustres reconstruisent du terrain de retenue ; elles ne
constituent pas une nouvelle simulation physique avec bilan de sédiments.
Leur naturalité reste une limite séparée de la continuité numérique.

## Vérification numérique reproductible

```sh
cargo run --release -p world-core --example quality_probe -- 128
cargo run --release -p world-core --example quality_probe -- data/worlds/<id>/atlas.json
cargo run --release -p world-core --example quality_probe -- data/worlds/32ede09abc580c1b3049/atlas.json --overlap-reference=examples/overlap-regression-2386084681.json
```

Le programme emploie la graine 2386084681 lorsqu'il génère un atlas. Il compare
les échantillons complets aux points partagés des tuiles L0 à L24, mesure les
différences de hauteur autour des frontières de support lacustre et examine
des profils au centre des 512 plus grands tronçons fluviaux. La côte d'une
fenêtre de 10 km est aussi échantillonnée sur sept grilles imbriquées,
de 8 à 512 intervalles (1 250 à 19,5 m).
Il n'effectue aucune mesure du GPU ou des images du navigateur.
Il cherche aussi les changements de propriétaire aquatique sur 500 100
positions proches des lacs, puis les localise à moins de 1 mm pour comparer
les **surfaces d'eau**, pas seulement le sol. Un contrôle séparé quantifie
le soulèvement imputable à la reconstruction des réservoirs.

Mesures de l'atlas final `32ede09abc580c1b3049`, graine 2386084681, 512²
par face ; deux parcours commencent sur une côte et au centre de la caméra :

| Contrôle | Résultat |
| --- | ---: |
| Points communs parent/enfant contrôlés, L0→L24 | 1 200 |
| Échantillons complets différents à ces points | 0 |
| Paires aux frontières de support lacustre | 39 750 |
| Différence maximale pour 3,47 cm de pas nominal | 13,9 mm |

Les différences lacustres sont des différences finies incluant la pente du
terrain, pas une mesure de saut discontinu. Les grilles côtières imbriquées
conservent leurs points communs. Un nombre de composantes raster différent
peut néanmoins révéler un chenal sous-échantillonné : cette mesure ne démontre
pas une invariance topologique de tous les polygones à toute résolution.
Dans cette fenêtre côtière, l'écart de classification marine entre les
triangles de 8, 16, 32 et 64 intervalles et la grille de référence de 512
vaut respectivement 3,00 %, 1,02 %, 0,34 % et 0,11 %. Cette mesure affine
ne reproduit ni la projection de la sphère ni le shader du navigateur.

L’analyse initiale a détecté des profils fluviaux trop profonds : 80,96 m
sur l’atlas 128 et 111,64 m sur le 512, contre environ 2 m attendus.
Le catalogue compile maintenant un profil spatial monotone à 128 segments
par reach, ajusté au même terrain naturel. Les niveaux des nœuds sont
partagés et les contraintes propagées sur le graphe ; les contrôles mer/lac
restent fixes. Les reaches s’arrêtent à la première rive d’un réservoir aval.
Le début d'une sortie de lac utilise la dernière rive ascendante avant
son contrôle, pour ne pas confondre un îlot intérieur avec le déversement.
Dans une section mouillée, une vallée sèche voisine ne peut plus creuser
le lit en laissant au-dessus une autre surface d’eau.
Trois passes de catalogue plafonnent les profils sous la surface des autres
vallées au centre et aux bords mouillés. Le propre reach est exclu ; les
surfaces libres remplacent les lits comme borne dans les sections mouillées,
pour éviter une baisse répétée d'une profondeur à chaque passe. Les nœuds
et contrôles restent fixes. Le sampler utilise ensuite une partition
continue des couples lit/profondeur, avec support complet dans le chenal
et décroissance sur les berges ; les propriétaires ne choisissent plus
les hauteurs. Les potentiels signés des berges empêchent une bosse quand
deux sections partagent le même plan d'eau.

Après intégration, le contrôle 512 porte sur **15 872 positions de 512 grands
reaches** : profondeur fluviale maximale **2,6701 m**, conforme à la section
canonique, aucun niveau dépassant le terrain naturel de plus de 10 m,
écart positif maximal 0,9312 m. L'ancienne priorité binaire produisait
des marches réelles de sol de 19,58 m et d'eau de 1,10 m aux confluences.
Le contrôle final reprend les dix voisinages initiaux, même si leurs
anciens mismatches ont disparu : **28 980 points**, **620 frontières de
propriétaire ou de mouillage**, maximum du sol **0,061 mm**, aucun saut
d'eau mesurable à la précision `f32` sur les 106 paires mouillées.
La profondeur locale maximale vaut 2,1408 m. La borne du mélange est la
profondeur maximale des contributeurs ; l'identifiant choisi n'en définit
pas le budget.

Le champ mélangé diffère encore du profil individuel sur 57 des 15 872
positions, maximum 3,3062 m. Le propriétaire y est identique : ce sont
des écarts entre catalogue et reconstruction, pas des sauts de propriétaire.
La partition garantit continuité et profondeur bornée ; elle ne constitue
pas un solveur hydraulique ni une preuve de monotonie du champ final.
Ce contrôle porte sur un monde et les reaches principaux, pas sur
une preuve universelle de tous les chenaux.
Un début de tronçon à l'intérieur d'un réservoir peut légitimement être
profond. Le probe publie séparément la profondeur de tous les plans d'eau
et celle des rivières hors mer et lacs, afin de ne pas confondre ces cas.

Les surfaces aquatiques ont aussi révélé un défaut que la continuité du
sol ne détectait pas : jusqu'à 101,8 m de différence entre deux plans
humides voisins. Le domaine métrique et les contrôles de déversement
suppriment ces grands sauts dans le relevé final. Le backwater adaptatif
des entrées lacustres corrige aussi les raccords latéraux résiduels.
La sonde relève **zéro saut de propriétaire supérieur à 10 cm** et aucune
différence d'eau mesurable à la précision `f32` aux changements examinés
sur les 500 100 positions proches des lacs. Les 1 200 échantillons complets
parent/enfant restent identiques.
Le [rapport de revue lacustre](LAKE_REVIEW.md) expose les corrections et
la limite de naturalité du terrain de retenue ajouté.
Le catalogue froid prend environ 5,43 s pour les caps seuls ; la sonde
finale mesure 9,87 s pendant les compilations native/WASM concurrentes.
L'analyse finale prend 8,89 s sous cette même charge. Ces mesures ne
décrivent pas le coût d'une tuile déjà servie depuis le cache.

## Complément minimal pour la qualité 10 km

L'étape suivante doit simuler une région géographique fixe, pas la tuile
visible : clé spatiale canonique, marge, conditions d'altitude et de pente
héritées du parent, entrées et sorties hydrologiques partagées, puis une
évolution locale à un pas de quelques dizaines de mètres. Les voisins
doivent utiliser les mêmes contrôles aux frontières. Un simple fondu vers
le parent évite un saut de hauteur, mais ne garantit pas que les cours d'eau
des deux régions se rejoignent.

Le résultat doit être calculé et sauvegardé avant publication, servi depuis
un cache borné, puis échantillonné à tous les LOD. Ses petits détails peuvent
être filtrés au rendu ; sa côte, ses vallées et ses connexions hydrologiques
doivent conserver les mêmes coordonnées. Cela ajoute un vrai réseau local
sans déplacer une crique ou un affluent chaque fois que l'utilisateur zoome.
Il ne faut pas présenter le prototype actuel comme équivalent à cette
simulation locale tant que cette étape et ses raccords n'ont pas été mesurés.

## Complément : petits lacs canoniques (prototype 0.9)

Le prototype reconstruit maintenant des bassins de 50 m à 3 km dans des provinces froides, humides et peu inclinées. Leur présence, leur contour et leur plan d'eau sont fixés dans le monde ; une maille plus fine résout des objets déjà présents. Les rivières et les réservoirs de l'atlas gardent leurs contraintes hydrauliques.

### Référence terrain à 10 × 10 km

La référence examinée est le TypeScript actif de `city_generator/web/src/gen/terrain/`, dans `/mnt/d/Workspace/Self/RPG/city_generator`. Les sources Python historiques n'interviennent pas dans cette comparaison.

| Source active | Apport utile au prototype sphérique | Limite du transfert |
| --- | --- | --- |
| `heightfield.ts` | Longueurs d'onde et hauteurs exprimées en mètres ; relief sublinéaire avec l'étendue ; sols doux autour des vallées | Le relief est normalisé pour une carte locale et utilise un côté aval choisi pour cette carte. Refaire cette normalisation au zoom changerait le monde. |
| `erosion.ts` | Incision stream-power implicite, drainage avant incision, relaxation des talus et diffusion ; variation d'altitude remontée depuis un solveur grossier | À 10 km, le domaine de simulation avec marge vaut 14,4 km et sa grille de 224 cellules donne environ 64,3 m. Ces bords locaux ne constituent pas les exutoires d'une planète. |
| `hydrology.ts` | Connexion marine par flood-fill, graphe de drainage acyclique, accumulation, géométrie de chenal et meanders selon la pente | Des apports ou chemins peuvent être forcés depuis le bord local. Le prototype doit garder le débit et les récepteurs du graphe global. |
| `rivernet.ts` | Largeur liée à l'aire contribuante, confluences orientées vers l'aval, raccords des canaux | Les aires virtuelles et les apports hors carte sont adaptés à une carte de jeu, pas à un bilan hydrologique planétaire. |

La reconstruction conserve les longueurs en mètres et les contraintes globales. Les champs géographiques sont évalués à une position fixe ; la caméra ne définit ni domaine de relief, ni graine, ni seuil d'apparition. Les provinces de bouclier atténuent le bruit fin au cœur du domaine sans effacer le relief de l'atlas. Leurs nouveaux petits bassins sont une hypothèse de reconstruction glaciaire, pas le résultat d'un solveur de glacier ou d'un nouveau bilan de l'eau.

### Objets lacustres et domaines

`local_water.rs` propose des centres irréguliers dans une fine coquille d'adresses spatiales 3D. Les buckets servent uniquement à retrouver les objets : aucune bordure de bucket n'entre dans les hauteurs ou les niveaux d'eau. Les formes réunissent quatre ou cinq lobes le long d'un axe courbe, parfois une branche latérale, puis un déplacement et une irrégularité continus du contour. Les tailles varient de 50 m à 3 km ; la profondeur nominale est bornée à 0,65–8 m. Les champs ne dessinent pas des disques réguliers ou un anneau de relief ajouté autour de chaque lac.

Chaque objet décide sa présence dans un lobe central réel, à partir de la province, de l'humidité annuelle, de la température annuelle, d'une pente de l'atlas et des contraintes de l'eau existante. Chaque lobe, y compris le bras latéral, fournit son centre et quatre rayons sondés sur le contour de rive et le contour extérieur du raccord. Ces sondes excluent les versants incompatibles avec la profondeur du bassin ; l'écart de hauteur autorisé entre les sondes vaut `0,5 + 0,02 × length_m` mètres. Le plan horizontal est fixé sous les altitudes du centre, de la rive basse et du raccord extérieur. Le relief intérieur et la frange sèche sont raccordés continûment au terrain naturel ; une frange humide alimente le mélange de biome marécageux. Entre deux plans différents, un partage sec continu évite une transition d'eau mouillée à un autre niveau. Une dépression sèche extérieure située sous ce plan conserve son propre drainage ; elle ne reçoit pas le datum du bassin voisin.

`roughness` est une pente en mètres par mètre ; les altitudes, dimensions et profondeurs sont en mètres. Les identifiants des petits lacs occupent `[1 << 40, 1 << 41)`, distincts de l'océan, des réservoirs physiques et du marqueur des rivières `1 << 52`. Le niveau associé à un identifiant ne dépend pas de la maille de rendu. Le sampler garde la priorité des grands réservoirs et de la mer ; le relief de ces domaines ne doit pas être repris par un petit bassin.

Le cache retient au plus 1 024 voisinages de 4 096 m et 65 536 propositions appartenant à la coquille sphérique. Un point dont le voisinage est prêt prend un seul verrou ; la construction d'un voisinage fait aussi des accès au cache des propositions. Les slots `Arc<OnceLock<…>>` partagent une validation froide entre appels concurrents, et les rejets sont mémorisés comme les objets retenus. Les callbacks de terrain sont exécutés hors du mutex. Plusieurs voisinages réutilisent le même descripteur `Arc<Basin>` et ne répètent donc pas toutes ses sondes de rive. Les deux caches utilisent une éviction FIFO bornée ; un descripteur partagé reste valide pendant les appels qui le détiennent. L'éviction entraîne un recalcul déterministe. Un clone de `World` partage ces caches tant que `invalidate_geometry()` n'est pas appelé ; un clone direct de `LocalWaterAtlas` part vide.

Les objets sont adressés dans les coordonnées de l'atlas `q`, puis transportés par la même côte fixe que les autres reliefs. Le callback retrouve le terrain géographique naturel sans microbassins ni creusement de rivière. Le halo couvre les supports des candidats et leur projection sphérique ; la garde de compilation rejette aussi en release tout support atteignant 2 300 m. Les objets sont triés par identifiant avant les mélanges, et chaque contribution de terrain est pondérée par son support continu, propriétaire compris. Les points communs à des buckets différents conservent ainsi le même ordre arithmétique. Le halo provincial de 2 500 m permet à un objet de dépasser la limite de sa province ; cette limite ne coupe pas un objet déjà accepté. Le sampler garde la priorité des plans physiques et utilise un index global des réservoirs et contrôles de déversement, avec une exclusion conservatrice à 2 301 m. Les provinces sont préselectionnées sur une pente d'atlas inférieure à 0,025 et à plus de 8 km des contraintes lacustres ; leur cœur conserve 4 % du détail fin, sans effacer le relief de l'atlas.

### Mesures du 3 octobre 2026

Mesure principale après revue et corrections, sur l'atlas 512 sauvegardé :

```sh
cargo test -p world-core local_water --lib -- --nocapture
cargo run --release -p world-core --example local_water_probe -- data/worlds/d8d1261ca22a1815f74f/atlas.json /tmp/wb-local-water-512-province-final
```

Ce fichier conserve la graine `2386084681`, un atlas `512² × 6`, 8 étapes d'érosion, 350 000 ans géologiques, un rayon de 1 737 km, un détail de 140 m et une densité lacustre de 0,55. La reconstruction locale courante est appliquée au sampling ; le chargement du JSON ne régénère pas l'atlas. Les 32 provinces sont explorées avec 81 points espacés de 1 250 m chacune. La fenêtre retenue est centrée sur la province 27, à la direction `[-0.5590794587440743, -0.774891421916368, -0.29491260239410366]`.

| Grille sur 10 km | Espacement | Identifiants avec un point mouillé | Part des points occupée par l'eau | Profondeur maximale | Points partagés modifiés | Temps de sampling |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 128 intervalles | 78,125 m | 38 | 5,6787 % | 5,591 m | 0 | 86,315 ms |
| 256 intervalles | 39,0625 m | 42 | 5,6867 % | 5,598 m | 0 | 326,930 ms |
| 512 intervalles | 19,53125 m | 42 | 5,6922 % | 5,603 m | 0 | 1 309,836 ms |

Les 82 690 comparaisons de points communs entre deux grilles conservent bit pour bit sol, eau, identifiant et statut lacustre. Tous les points mouillés d'un même identifiant partagent le même plan horizontal. La grille fine comporte aussi 4 588 points classés marécage, soit environ 1,74 %. Les proportions sont des comptages de points dans cette fenêtre choisie, pas une statistique de la planète ni un bilan d'eau. Les quatre identifiants supplémentaires entre les grilles grossière et fine étaient sous-résolus, sans activation liée au zoom.

Les coûts sont séparés : lecture du JSON **2 249,191 ms** ; première compilation complète du catalogue de rivières déclenchée par `world.river_reach(0)` **3 650,228 ms** ; exploration des 2 592 points provinciaux **1 790,711 ms**. Une première exécution de la même compilation froide mesurait 3 925,924 ms. Ce coût hydraulique comprend les caches géographiques et lacustres que la compilation consulte, et n'est pas un chronométrage isolé de `local_water`. Les temps des grilles mesurent seulement les appels à `world.sample`, après cette préparation ; l'écriture des CSV en est exclue.

Le premier probe plaçait sa fenêtre sur le dernier point mouillé du survey, environ 5 km à côté de ce centre provincial : cette fenêtre contient 21 identifiants et 3,0737 % d'eau à 19,53 m, avec la même cohérence des points partagés. L'exemple a été corrigé pour centrer la fenêtre sur la province sélectionnée. Un contrôle dans la fenêtre antérieure exacte `[-0.10790962020745694, -0.9008999688800017, -0.42039833484291345]` donne 8 identifiants, 1,1084 % d'eau et 4,782 m de profondeur à 19,53 m, toujours sans modification des points partagés. Les CSV et rapports complets sont dans `/tmp/wb-local-water-512-province-final*`, `/tmp/wb-local-water-512-final*` et `/tmp/wb-local-water-512-old-final*`.

La carte numérique des trois résolutions est dans `/tmp/wb-local-water-512-center-final.png`. La coupe du bassin `1874456003376`, au point local `x = -1 972,65625 m`, `y = 3 007,8125 m`, montre un fond à 267,076 m sous le plan horizontal à 272,679 m. Elle est conservée dans `/tmp/wb-local-water-512-center-final-profile.png`. Ces figures sont dérivées des CSV et n'évaluent pas le renderer.

### Mesure 128 antérieure à la revue, conservée comme comparaison

Le tableau suivant décrit le premier essai, avant les sondes par lobe et par raccord extérieur, la protection globale des plans physiques et le cache par proposition. Il ne décrit pas le champ final et n'est pas directement comparable à l'atlas 512, dont le nombre d'étapes d'érosion diffère. Le programme peut encore générer un monde 128 avec `cargo run --release -p world-core --example local_water_probe -- 128 /tmp/wb-local-water-real`, mais la reconstruction actuelle ne reproduira pas ces résultats historiques.

L'exemple génère la graine `2386084681`, un atlas `128² × 6`, 25 étapes d'érosion et les autres valeurs par défaut de cette révision : rayon 1 737 km, temps géologique 350 000 ans, détail 140 m, densité lacustre 0,55. Il mesure les huit premières provinces par 81 points espacés de 1 250 m et examine une fenêtre de 10 km dans la plus riche de ces huit zones. Ce choix ne modifie aucun objet du monde. La compilation de l'atlas prend 2 446,925 ms sur cette machine. La première exploration des huit provinces prend 3 813,564 ms pour 648 points ; ce coût initial comprend les caches géographiques et hydrauliques froids et ne représente pas un coût isolé du module lacustre.

Le monde mesuré a l'identifiant `f4b66658bad7e5c3ac1d`. La fenêtre choisie se situe dans la province numéro 7, autour de la direction `[-0.4851244680608404, 0.8332524680089498, 0.26522551733511784]`.

| Grille sur 10 km | Espacement | Lacs avec un point mouillé résolu | Part des points occupée par l'eau | Profondeur maximale | Points partagés modifiés | Temps de sampling |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 128 intervalles | 78,125 m | 27 | 4,7173 % | 4,686 m | 0 | 188,421 ms |
| 256 intervalles | 39,0625 m | 33 | 4,7404 % | 4,695 m | 0 | 383,804 ms |
| 512 intervalles | 19,53125 m | 34 | 4,7418 % | 4,715 m | 0 | 1 580,865 ms |

La comparaison porte sur la hauteur du sol, la hauteur de l'eau, l'identifiant et le statut lacustre, exactement aux points communs. Chaque lac mouillé conserve un niveau identique bit pour bit. Les sept identifiants supplémentaires entre 78 m et 19,5 m sont des objets sous-résolus par la première grille ; cela ne prouve pas la conservation de la topologie des triangles entre les points. Sur la grille fine, 3 886 points secs sont classés comme marécage, soit environ 1,48 % des points de la fenêtre. Les CSV et le JSON complet sont produits dans `/tmp/wb-local-water-real*`.

Le bassin le plus profond a l'identifiant `1350471602600`. Au point local `x = 4 140,625 m`, `y = 1 113,28125 m`, son fond est à 414,130 m et son eau à 418,845 m. La coupe garde un plan horizontal au-dessus d'un fond irrégulier ; la profondeur maximale mesurée est de 4,715 m. Des figures Matplotlib dérivées des CSV ont été inspectées dans `/tmp/wb-local-water-real.png` et `/tmp/wb-local-water-real-profile.png` ; ce sont des figures du champ numérique, pas des captures du renderer.

### Régressions du module final

Les huit régressions du module passent. Sur une province synthétique plate et humide à densité 1, 1 135 points sur 6 561 sont mouillés, avec 75 identifiants résolus à 125 m. Après vidage des deux caches et parcours inverse, identifiants, plans et altitudes restent identiques. Sur un sol doux avec pente et bruit continus, 104 changements de propriétaire entre plans différents sont bisectés à moins de 4 µm : aucune paire ne reste mouillée des deux côtés et le saut de terrain maximal vaut `0,000000446 m`. Aux 203 entrées ou sorties d'un support voisin, la même bissection mesure un saut maximal de `0,000000226 m`. Des paires à 2 mm sur une véritable frontière de cache restent sous 1 cm de différence. Les provinces sèches, chaudes, raides ou protégées n'ajoutent aucun objet. Sur 512 points d'intégration répartis entre une mer et un grand lac synthétiques, activer le module conserve exactement lit, plan, identifiant et statut. Deux régressions concurrentes vérifient qu'un quartier froid et deux quartiers différents qui se chevauchent exécutent le même nombre de callbacks qu'un parcours série ; les propositions retenues et rejetées sont réemployées après vidage du seul cache de quartiers.

Ces contrôles portent sur le champ déterministe et des cas précis. Les microbassins restent une reconstruction sans bilan local vérifié de précipitation, d'évaporation, de stockage ou de sédiment. Le rendu WebGPU, les interactions de chaque domaine lacustre sinueux et l'érosion glaciaire réelle doivent être évalués séparément.

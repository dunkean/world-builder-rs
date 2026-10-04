# Biomes continus et textures de sol

Le rendu « Biomes et eaux » mélange des matières colorées, au lieu de
multiplier une teinte uniforme par un grain de luminosité. Les huit
fractions terrestres du sampler arrivent séparément au GPU : glace,
toundra, désert, prairie, forêt, forêt tropicale, marais et roche. Les
fractions océaniques et lacustres ne participent jamais au mélange du sol.

## Motifs à plusieurs échelles

Six champs 3D fixes, de 64 km, 8 km, 1 km, 125 m, 16 m et 2 m, construisent
des mosaïques imbriquées. Chaque octave fournit trois champs de gradients
indépendants : les hachages entiers choisissent aux coins parmi douze
directions unitaires, puis une interpolation quintique mélange les
produits scalaires locaux. Cela évite les plateaux carrés du bruit de
valeurs et garde des transitions lisses.
La grille est tournée par rapport aux faces du cube ; le champ de l'octave
précédente déforme son domaine. Les motifs traversent les faces et les
tuiles sans utiliser leurs UV, leurs clés ou leur niveau de détail.

Les champs changent les proportions entre des encres distinctes : herbe
verte et sèche, sol nu et mousse, canopée et clairières, sable et minéraux,
roseaux et tourbe, neige et glace ancienne. Même un échantillon contenant
100 % de forêt, de désert ou de roche reçoit ces variations de couleur.
La pente physique augmente la présence de roche ; température et humidité
guident les matières compatibles avec le biome. Une région glaciaire
froide conserve sa neige et sa glace, avec des variations bleutées et
ivoire ; les marges plus chaudes et les faces abruptes exposent des débris.

La forêt de plaine alterne une canopée vert foncé et des clairières vert
clair à plusieurs échelles. Le coefficient de canopée commence à décroître
à 20° et atteint zéro à 55°. L'accumulation de neige diminue entre 25° et
60° ; toute matière devient roche nue à partir de 65°. Ces angles viennent
des normales de terrain, après inversion de l'exagération verticale :
`tan(pente_physique) = tan(pente_rendue) × (R + e·h) / (R + h) / e`.
Une montagne affichée cinq fois plus haute conserve donc ses coefficients
de couverture physique. La normale reste celle du maillage résolu et
change progressivement pendant son morphing.

Dans les écotones, les fractions interpolées entrent en compétition douce
avec ces mêmes champs. Les transitions se fragmentent en langues et en
îlots, avec des bords continus. Le relief et le classement hydrologique
ne sont pas modifiés par ces couleurs. Le sampler doit fournir une
transition terrestre progressive : un shader ne peut prolonger une
fraction strictement nulle au-delà d'une frontière catégorique.

## Raffinement progressif au zoom

La taille d'un pixel en mètres vient des dérivées du point de surface
précis. Une octave apparaît progressivement entre huit et deux pixels
par longueur caractéristique ; son calcul est sauté en dessous de deux
pixels. Les cartes lointaines montrent les grandes mosaïques, et les
zoom rapprochés révèlent les matières plus fines.

Le filtrage porte sur la contribution colorée, jamais sur les coordonnées
du champ. La déformation du domaine utilise la valeur non filtrée de
l'octave parente : un changement de zoom ne déplace pas les petits motifs
déjà visibles. Le temps, l'écran, le niveau de tuile et l'exagération du
relief n'interviennent pas dans leur phase. Les modes de diagnostic
altitude, température, humidité et drainage gardent leurs rampes.

## Précision jusqu'à 2 m

Une direction planétaire `f32` seule ne suffit pas au détail métrique.
Chaque sommet transporte aussi son résidu de direction, calculé en
`f64`. Le vertex shader soustrait une origine scindée avant de convertir
la différence locale en mètres. L'origine fournit, pour chaque octave,
un identifiant entier de cellule mondiale et une petite phase fractionnaire
calculés en `f64` côté Rust.

Une translation de l'origine de calcul conserve ainsi le champ mondial.
La projection de la direction interpolée sur la sphère utilise une
expression locale qui évite la soustraction de grands nombres presque
égaux. Elle évite également de faire dépendre la phase du triangle courant.
Le test de précision vérifie un mouvement de 25 cm et un déplacement
d'origine de 2 km sur une planète de rayon 6 371 km, avec moins de 4 mm
d'écart de phase à l'échelle de 2 m. Il ne certifie pas une texture au
centimètre à toute distance.

## Côtes et plans d'eau

La teinte de berge appliquée auparavant aux 14 premiers mètres au-dessus
de la mer, ou aux trois premiers mètres au-dessus d'un lac, est retirée :
une altitude proche d'un plan hydraulique ne prouve pas la présence d'une
plage. Les échantillons de fond classés uniquement comme eau reçoivent
une encre terrestre de secours selon leur climat ; ils ne créent plus une
frange artificielle de marais.

Le texturage conserve positions, clipping hydraulique, profondeurs, flags,
epochs et offsets du morphing. L'eau est toujours rejetée à profondeur
`<= 0` ; mer, lacs et rivières gardent leurs encres de profondeur et leurs
calques. La sortie reste opaque. Une anomalie de triangle côtier doit
être contrôlée sur la géométrie, indépendamment de ces encres.

Le triangle côtier levé avait une cause distincte : aucune règle ne
limitait l'écart de niveau entre voisins. Le raccord d'une bordure fine
sur la corde d'une tuile beaucoup plus grossière pouvait soulever le fond
marin, puis masquer son eau par une profondeur grossière négative. Une
régression reproduit plus de 30 m de soulèvement sur un rivage analytique
dont la pente maximale est seulement de 0,119 m/m ; le raccord à un voisin
du niveau immédiatement inférieur déplace ce point de moins d'un millimètre.

Les parents à subdiviser sont maintenant fermés sous leur support 2:1.
Le chargement demande ce support ; les subdivisions et regroupements
vérifient aussi la partition proposée. Un centre déjà en cache ne peut
dépasser une couronne encore froide. Les cibles de niveaux mixtes cessent
de conserver tous les anciens descendants d'une tuile de support.
La contrainte vaut pendant les transitions comme sur la coupe finale ;
elle borne l'erreur de raccord à un seul niveau, sans déplacer le plan de
mer ni peindre les triangles en eau.

## Format, mémoire et coût

Le paquet préparé passe de `WVP1` à `WVP2` ; le décodeur rejette un ancien
paquet. Les samples JSON et Postcard restent compatibles. Le cache worker
stocke les tuiles natives, pas les paquets préparés, et refait leur
préparation avec le WASM chargé.

| Donnée par sommet | Avant | Maintenant |
|---|---:|---:|
| Sommet CPU préparé | 112 octets | 156 octets |
| Buffer statique GPU | 72 octets | 116 octets |
| Buffer dynamique GPU | 44 octets | 44 octets |
| Total attributs GPU | 116 octets | 160 octets |

Le surcoût est de 44 octets par sommet ; l'uniforme de précision ajoute
224 octets par scène, plus 16 octets pour l'exagération physique. Les
budgets de cache continuent à compter les tailles réelles des structures
et des buffers, y compris les 32 octets de bornes 3D conservées par tuile
et toute sa structure de cache. La borne de calcul du sol est de
six octaves, soit 48 coins avec trois canaux par coin. L'eau n'évalue que
les trois premières octaves. Les octaves filtrées
sont sautées ; les conversions de palette n'ont pas de lookup de texture.
La croissance de mémoire et les FPS doivent être mesurés dans le navigateur,
avec les mêmes scènes et dimensions que la référence.

Les tuiles des niveaux 1 et 2 gardent 64 segments ; à partir du niveau 3,
elles utilisent 32 segments. Le niveau demandé est augmenté d'un cran,
ce qui conserve l'espacement théorique `2R / (segments × 2^niveau)` avant
projection sur la sphère. Le passage 64→32 repartitionne les mêmes sommets,
y compris le plan marin et les profondeurs. Les paquets de 64 segments
restent acceptés pour le générateur historique `contracts`, dont la graine
dépend de ce nombre.

La planification borne la coupe complète graduée à 1 024 feuilles, pour
réserver les parents inverses, les racines et les quartets de chargement
dans les budgets existants de 768 MiB CPU et GPU. Elle réduit au besoin le
niveau avant de construire la fermeture, sans retirer de support. Les
champs `requested_level`, `delivered_level`, `detail_limit_reached` et
`planned_leaves` rendent ce compromis mesurable. Le nombre réellement
résident et ses octets restent affichés séparément.

La coupe active reste une partition complète. Les appels de dessin sont
filtrés par les sphères englobant les vrais sommets 3D du terrain et de
l'eau, élargies de tous les offsets de morphing, dans le repère de la
caméra effectivement rendue. Cette optimisation ne modifie ni la coupe
ni les raccords et n'utilise pas une emprise calculée sur une planète plate.

Le cache des voisins de transition utilise la contrainte 2:1 pour sonder
les quarts de bord et les coins, au lieu de parcourir toute la coupe.
Pendant un batch de regroupements, les voisins déjà regroupés ne peuvent
que devenir plus grossiers : la garde du voisinage affiché reste une
borne conservative pour la partition proposée. Les groupes dont les
zones de travail sont disjointes peuvent ainsi partager une même
animation, toujours limitée à huit parents et 0,16 seconde.

Un regroupement termine immédiatement si les bornes réelles du parent,
de ses quatre enfants et des voisins affectés restent toutes hors du
frustum, ou si leur union occupe au plus un demi-pixel. Ces bornes couvrent
le terrain, les plans d'eau et les déplacements de morphing. Le diamètre
projeté tient compte de la profondeur et de la position hors axe ; une
borne traversant le plan de la caméra conserve l'animation. La décision
est revérifiée après la préparation, pour qu'un changement de zoom puisse
réactiver l'animation. Le compteur `instant_coarsen_commits` permet de
mesurer cet effet sans ajouter de sondage par image.
Les tuiles utilisées pour placer le point visé et l'œil incliné gardent
leur animation, même hors champ. Un emplacement de l'anneau des 32 epochs
ne peut être réutilisé avant la fin de son morphing ; une préparation
annulée libère son emplacement sans réinitialiser les identifiants.
Lors d'un effacement explicite du cache, les racines conservées retrouvent
ensemble leurs offsets, normales et profondeurs hydrauliques canoniques,
y compris si un raccord avait déjà été préparé pour une coupe abandonnée.

## Validation

Les 61 tests du viewer passent. Ils couvrent le mélange terrestre excluant l'eau et son
secours climatique, la phase métrique lors d'un déplacement
sous-métrique ou d'un changement d'origine, l'inversion de pente à ×1,
×5 et ×30, et le raccord exact terrain/eau 64→32. Les coupes graduées sont
vérifiées près des limites de quadrant, des arêtes et des coins de toutes
les faces du cube, ainsi que pendant le raffinement et le retour arrière
d'un centre chaud entouré de support froid. Le même scénario charge un
seul quartet réservé à la fois, comme sous pression mémoire.
Les tests de berge vérifient l'absence de halo et la conservation des
niveaux hydrauliques ; le test du paquet vérifie le rejet de `WVP1`.
L'oracle indépendant compare exactement les voisins locaux au parcours
complet sur les six faces et lors de batches successifs de regroupements,
puis vérifie la graduation de chaque partition proposée.
Les bornes de projection sont comparées à des points de sphère hors axe,
avec plusieurs inclinaisons et tailles de canvas ; une scène de rivage
vérifie leur couverture du relief, de l'eau et du morphing à ×1, ×5 et
×30, puis le refus d'un saut après un retour au zoom rapproché.
La protection des tuiles caméra est vérifiée à 35° et 60° avec un nadir
hors frustum ; une séquence à 240 Hz vérifie l'attente du slot epoch actif
avant son recyclage. Les repères caméra non finis sont refusés.
L'annulation en préparation conserve les profondeurs et normales des
racines ; le repère caméra et sa matrice de rendu restent finis aux deux
pôles, avec un axe de secours commun.

Naga 27 valide le WGSL et sa traduction GLSL ES 3.00 pour WebGL2.
Le build WASM passe après intégration du sampler 0.10. La
[matrice finale de seize scènes](RENDERING_0_10.md) sur RTX 3090, en
918×773, conserve un p95 rAF d’environ 16,8 ms sans erreur GPU ni
longue tâche et respecte l’écart de LOD ≤1 ainsi que les budgets
de 768 MiB. Les deux scènes côtières proches sont à 5 m, minimum
réel de la caméra. Le retour orbital prend 15,948 s avec 154 commits,
dont 65 immédiats, contre 27,735 s avant ce critère. La revisite à
500 m prend encore 21,116 s : la fluidité ne certifie pas un
chargement instantané, et une revisite après plusieurs régions
peut avoir perdu ses tuiles utiles. Le rapport précise l’état des
caches, les intervalles rAF et les limites des mesures.

La sonde ponctuelle `surface_probe(direction_json)` expose le triangle
de terrain réellement utilisé par la coupe affichée : sommets canoniques,
hauteurs affichées, offsets et progrès du morphing, voisins de bord,
hauteur du raccord grossier, profondeur et niveau de mer. Elle utilise
le temps de la dernière image soumise et ne coûte rien dans le rendu courant.
Elle sert à distinguer un vrai soulèvement de sommet d'une variation
d'éclairage ou de matière au très fort zoom côtier.

`surface_edge_audit()` inspecte ponctuellement toutes les bordures actives
et renvoie l'écart maximal de LOD, la paire responsable et huit sommets
ayant les plus grands déplacements, avec priorité aux sommets dans le
frustum. Son champ `visible` signifie « dans le frustum », sans test
d'occlusion par la planète : une bordure grossière derrière le globe
peut figurer dans ce diagnostic, même lors d'un zoom sur la face opposée.
Elle permet de déplacer la caméra sur la bordure effectivement
concernée ; aucune de ces sondes ne tourne par image.

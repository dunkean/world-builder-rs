# Îles et relief côtier 0.10

Les côtes et les objets insulaires sont définis dans les coordonnées de l’atlas
figé. Le transport sphérique commun les amène dans le monde affiché. Aucun
coefficient, candidat, contour ou support ne dépend de la caméra ou du LOD.

## Un spectre insulaire au large et près des continents

Le catalogue côtier passe de 36 à 144 districts au maximum, avec jusqu’à
96 propositions par district. Les centres partent maintenant d’un vrai
croisement du niveau marin reconstruit, au lieu d’un décalage depuis un centre
de cellule potentiellement très intérieur. Les îles majeures vont jusqu’à
15 km de demi-grand axe ; les petits satellites commencent à 25 m.

Un deuxième champ ajoute des chaînes océaniques et des îles isolées. Il possède
sept bandes de demi-grands axes nominaux maximaux : **60 km, 16 km, 4 km, 1 km,
250 m, 62,5 m et 16 m**. Chaque proposition prend 28 à 100 % de la taille de sa
bande. Les quatre bandes les plus larges peuvent produire des groupes de trois
à six îles, disposés le long d’arcs courbes. Les bandes fines créent des
rochers isolés. Ces longueurs diminuent sur les planètes plus petites que le
rayon de référence de 1 737 km.

Un champ régional continu module les provinces océaniques ; une crête de bruit
continu fournit des arcs et leur direction. La profondeur de l’atlas renforce
les chaînes de plateforme, avec des concentrations régionales. Chaque bande
utilise une grille spatiale tridimensionnelle tournée indépendamment, des
centres irréguliers et un rejet radial autour de la sphère. La grille sert
d’adresse canonique ; ses points ne sont pas les centres d’une grille visible.
Le réglage existant `archipelago_density` contrôle toutes les propositions.

Les petits objets doivent trouver une plateforme, le pied d’un relief de
bande plus grande ou celui d’un archipel côtier figé : leur profondeur préalable ne dépasse pas deux fois leur
demi-grand axe. Cela évite de faire monter un rocher de dix mètres depuis une
fosse de plusieurs kilomètres. Leur profil soulève tout un relief, avec un
pied immergé et un raccord continu au fond d’origine. Le contour marin est
l’intersection de ce relief avec le plan d’eau, et non un masque de bruit
appliqué au niveau zéro.

## Supports, garde hydraulique et caches

Le support d’une île tient dans un disque de rayon `2,2 × demi-grand axe`.
Les lobes de son contour et le raccord externe restent dans ce disque. Chaque
objet doit conserver **3 000 m** de dégagement par rapport aux domaines
lacustres physiques et aux contrôles de déversement.

Le dégagement hydraulique existant est plafonné à 20 km. Les grandes îles
vérifient donc une couverture entière par disques : centres espacés de 18 km,
rayon de couverture `9 km × √2`, plus la marge de 3 km. La projection du plan
tangent vers la sphère contracte les distances de corde ; le dégagement est
1-Lipschitz. Chaque point du support possède ainsi le dégagement requis, même
si le centre de l’île est bien au-delà du halo de 20 km du réservoir.

Le champ hiérarchique compile seulement les candidats proches d’un bucket
demandé. Les 27 cellules voisines suffisent : le décalage radial maximal vaut
0,45 fois l’espacement et les chaînes, îles et supports réunis ajoutent moins
de 0,43 fois cet espacement. Les calculs restent dans un ordre canonique.
Le cache contient au maximum **512 buckets par bande**. Une éviction ou une
compilation concurrente reconstruit exactement les mêmes objets.

Un front de **8 buckets par bande et par thread** évite les verrous partagés
pour les voisinages répétés. Il conserve une référence propriétaire au catalogue
utilisé : une adresse d’allocation réutilisée, ou deux mondes avec la même
graine et des atlas différents, ne peuvent pas mélanger leurs objets.

Chaque bande possède aussi une borne de hauteur : le pic maximal multiplié
par 1,25, qui borne sa texture. Lorsqu’un terrain dépasse cette borne, tous les
reliefs de cette bande produisent exactement la même hauteur que l’entrée.
L’évaluation peut donc être omise. Ce contrôle évite les microbuckets inutiles
dans les provinces lacustres intérieures sans modifier le champ.

## Fjords et détail des côtes

Les racines glaciaires suivent des courbes de Bézier, avec des points de
contrôle communs aux segments adjacents. Le nombre de racines, leurs positions,
les espacements des points, les départs des branches et leurs longueurs varient.
Les grandes vallées couvrent plusieurs kilomètres et possèdent une section
arrondie en U. Les branches rejoignent des points de racine communs ; leurs
fonds restent marins. Les îles sont soulevées avant la dernière passe de
creusement, afin de ne pas barrer un fjord.

Le support de chaque segment couvre l’enveloppe de ses quatre points de contrôle
et sa largeur complète. La même garde Lipschitz et la marge de 3 km protègent
les réservoirs physiques. Les nouveaux fjords sont des reliefs géographiques
fixes, pas une résolution d’érosion locale lancée au zoom.

Les twists côtiers existants conservent leur inverse exact. Le spectre des
bandes fines passe de H=0,68 à H=0,62 et son plafond de coefficient de
`0,45 × √ratio` à `0,60 × √ratio`. La première bande garde le plafond précédent.
Ce changement renforce la sinuosité fine ; le plafond borne le coefficient
angulaire, pas le Jacobien de toute la composition.

Les districts lacustres couvrent davantage de régions humides à pente faible :
latitude absolue au-dessus de 0,25, climat annuel entre −18 et 20 °C,
jusqu’à 96 provinces, rayons de 95 à 210 km. Le détail fin garde au minimum
12 % de son amplitude, contre 4 % auparavant. Les bassins locaux conservent
leurs propres vérifications de fond, de berge et de drainage.

## Vérification et limites

Les régressions natives vérifient les sept bandes de tailles, la couverture des
buckets aux limites, l’identité après éviction et entre threads, le dégagement
sur le support entier d’une grande île, le raccord externe des reliefs et des
vallées, et le caractère marin des centres des réseaux glaciaires générés.
Une référence sans omission de bandes vérifie l’identité bit à bit de la borne
de hauteur, pour des entrées de −100 à 6 000 m. Les cinq régressions des twists
vérifient toujours l’inverse, les axes, le détail métrique et la continuité.

Le microbenchmark natif isolé de 16 384 requêtes répétées dans un fond marin
à −2 m mesure le champ insulaire déjà en cache. Avant le front par thread,
il prend 7,24 ms sur un thread, 19,15 ms sur quatre et 24,56 ms sur huit.
Après correction, les durées sont **4,82 / 3,22 / 1,81 ms**. Les sommes de
hauteurs sont identiques. Cette mesure vérifie la contention du cache ; elle
ne mesure pas une tuile complète, le chargement d’un atlas ou le rendu GPU.

Le diagnostic natif ignoré `landform_size_spectrum_diagnostic` peut charger un
atlas figé avec `WB_LANDFORM_ATLAS`. Il rapporte le catalogue côtier et les
objets uniques des trois grandes bandes rencontrés par 2 048 directions
globales. Ces comptes décrivent des objets canoniques ; ils ne comptent pas
les composantes terrestres d’un raster et ne donnent pas le total mondial des
bandes fines implicites.

Les nouvelles îles ne recalculent pas le bilan global d’eau de l’atlas physique.
Le catalogue est un complément géographique du champ continu. Les fonds de
fjord et les contacts de relief restent échantillonnés par les tuiles : leur
apparence à un LOD grossier relève aussi du maillage. La vérification numérique
de ce champ ne remplace pas une inspection du rendu GPU.

# Design technique et opérationnel du monde procédural multi échelle

**Date : 1er octobre 2026. Statut : proposition d’architecture à implémenter et à valider.** Ce document complète [le cadrage](OBJECTIF_MONDE_PROCEDURAL.md). Il ne décrit pas une réalisation existante et ne reprend pas comme acquis les résultats des prototypes.

## 1 Décision proposée

Construire un **compilateur de monde hiérarchique**, associé à un explorateur planétaire. Le compilateur produit d’abord un atlas global de géologie, relief, climat et drainage. Il fige ensuite les contrats qui autorisent le calcul local de détails. L’explorateur parcourt cette représentation et demande ses raffinements ; il ne participe jamais aux décisions constitutives du monde.

Le système repose sur quatre composants :

1. **Un atlas physique global**, relativement petit, qui établit continents, chaînes, grands bassins, climat saisonnier et ressources.
2. **Des graphes et des objets hiérarchiques**, qui portent les fleuves, lacs, lignes de partage, côtes, implantations et connexions de transport.
3. **Un générateur régional contraint**, qui développe le relief et les attributs sans changer les contrats hérités.
4. **Un moteur de rendu avec streaming**, qui affiche immédiatement les données disponibles puis révèle les niveaux suivants.

La proposition centrale est un **raffinement par contrats de bassin et de frontière**. Un parent transmet des moments d’altitude, des enveloppes géométriques, des connexions et des budgets physiques. Les enfants disposent de liberté à l’intérieur de ces contrats. Une proposition d’érosion ou un réseau appris fournit une forme candidate ; un solveur et un certificat déterminent si elle appartient au monde défini.

L’architecture mélange des résultats publiés et des méthodes proposées ici. Les contrats combinés, leur construction locale et le solveur complet constituent une **hypothèse de recherche et d’ingénierie**, pas un algorithme déjà démontré par la littérature. La section 18 définit les expériences qui peuvent la réfuter.

### Résultat visé

Une planète de référence de rayon **1 737 km**, explorable du globe à un échantillonnage horizontal inférieur ou égal à un mètre dans les régions visitées. La topographie, les bassins et les objets sont reproductibles après effacement des caches. Les fleuves participent à la forme et aux matériaux du paysage, y compris lorsqu’ils traversent un désert.

Le terrain est une surface d’altitude avec roche, sédiments, sols et eau. Il n’y a pas besoin de voxels planétaires, de grottes, de bâtiments intérieurs, de récit ou de génération textuelle. Les silhouettes de végétation et de constructions sont des instances de rendu. Une surface métrique ne signifie ni exactitude géologique métrique, ni observation réelle de la planète.

### Choix retenus avec le demandeur

- **Prototypage : quelques minutes au maximum** pour examiner une graine et ses paramètres.
- **Monde retenu : quelques heures de préparation** acceptables pour figer les données globales et préparer les régions importantes.
- **Aucun entraînement obligatoire.** Un modèle appris est une extension évaluée contre le générateur de référence.
- **Nouvelles méthodes autorisées** lorsque les publications ne suffisent pas. Elles doivent avoir des contrats, un comportement d’échec et des critères de validation.
- **Rust pour le système de production** : compilateur, graphes, ordonnanceur, service et visualiseur. Les programmes GPU utilisent WGSL ou CUDA selon le backend, avec pilotage en Rust ; les outils scientifiques externes et l’entraînement optionnel ne participent pas au chemin interactif.
- **Explorateur web en Rust et WebAssembly comme cible principale**, avec application locale partageant le renderer si son avantage de performance est mesuré. Prévoir Windows et Linux/WSL pour le moteur.

## 2 Machine cible et limites de calcul

La session expose une RTX 3090 de **24 Go de VRAM**, un **i7-10700K avec 8 cœurs et 16 threads**, environ **32 Gio de RAM** et **150 Go libres** sur le volume du projet. L’environnement est virtualisé sous Microsoft ; CUDA et son compilateur sont présents. Ces observations ne constituent pas un benchmark. La mémoire disponible varie avec les autres travaux.

Pour le rayon de référence :

~~~text
Surface = 4 π R² ≈ 3,7915 × 10¹³ m²
Altitudes globales à un échantillon par m² en float32 ≈ 151,7 To
~~~

Ce chiffre est une estimation surfacique minimale ; une grille cubique garantissant un pas maximal d’un mètre peut contenir davantage d’échantillons. **Le terrain métrique global ne sera jamais matérialisé.** On conserve l’atlas, les règles, les contrats et des régions calculées à la demande.

Les trois temps de calcul sont distincts :

| Temps | Travail | Condition de reproductibilité |
|---|---|---|
| Prévisualisation | Modèle global réduit pour choisir un monde | Profil et résolution inclus dans sa version |
| Préparation | Atlas final, graphes globaux, contrats et régions prioritaires | Résultat figé et contrôlé avant publication |
| Exploration | Descente dans la hiérarchie, calcul régional et rendu | Même algorithme quel que soit le trajet |

La prévisualisation et le monde final sont **deux révisions explicites**. Ils partagent graine, continents proposés et paramètres ; la compilation finale peut déplacer des vallées et des implantations. On ne mélange jamais leurs tuiles. La permanence commence à la publication d’une révision finale, pas entre deux niveaux de fidélité du prototype.

## 3 Ce que la littérature apporte et ce qui reste à inventer

Les sources sont primaires, recherchées et vérifiées le 1er octobre 2026. Les références complètes figurent en section 20.

| Travail | Brique retenue | Limite pour ce projet |
|---|---|---|
| Génévaux et al. 2013 [R1][r1] | Réseau de drainage hiérarchique et primitives de terrain | Ne fournit pas tous les contrats planétaires, climatiques et conservatifs |
| Derzapf et al. 2011 [R2][r2] | Raffinement adaptatif d’une planète avec réseaux fluviaux | La permanence demandée ici inclut davantage de couches et de bilans |
| Fischer et al. 2022 [R3][r3] | Construire des bassins et de l’eau avant de développer le terrain | Le réalisme visuel n’est pas une validation physique complète |
| Cordonnier et al. 2016 [R4][r4], Schott et al. 2023 [R5][r5] | Soulèvement et incision pour les formes à grande échelle | Ne pas réexécuter l’histoire géologique à chaque tuile |
| Tzathas et al. 2024 [R6][r6] | Érosion analytique et accélération multigrille | Ne garantit pas seule la stabilité d’une hiérarchie de zoom |
| Schott et al. 2024 [R7][r7] | Amplification par processus d’érosion à plusieurs échelles | Ajouter les contraintes de frontière, de réduction et d’identité |
| Barnes et al. 2014 et 2021 [R8][r8], [R9][r9] | Dépressions, bassins et stockage avec débordement | Ne pas remplir systématiquement les lacs naturels |
| Perche et al. 2025 et Huftier et al. 2026 [R10][r10], [R11][r11] | Représentations vectorielles et reconstruction de surfaces | Les flux et les raccords restent à spécifier |
| Borg et al. 2026 [R12][r12] | Diffusion conditionnée pour les planètes | Aucune garantie suffisante de drainage et de permanence métriques |
| Paradise et al. 2022 [R13][r13] | ExoPlaSim pour comparer des climats planétaires | Modèle hors de la boucle de rendu, coût à mesurer |
| Galin et al. 2010 [R14][r14] | Routage de routes avec coûts topographiques | Les pôles, l’économie et les garanties de raffinement restent à concevoir |
| Asirvatham et Hoppe 2005 [R15][r15] | Grilles régulières, résidus et transitions de LOD | Le rendu ne produit pas la géomorphologie |

**Synthèse proposée ici :** placer le graphe hydrologique, les enveloppes et les bilans au-dessus des rasters ; employer l’érosion pour proposer du détail ; certifier ce détail avant de le publier. Une méthode apprise peut remplacer la proposition, jamais la définition des contraintes.

Il est inutile de chercher un article unique qui résout tout. Il serait tout aussi injustifié de considérer que l’assemblage des publications résout automatiquement les interfaces entre leurs modèles.

## 4 Définition du monde et invariants

### Identité et coordonnées

~~~text
WorldId = hash(
    graine, paramètres normalisés, versions des algorithmes,
    profil de compilation, empreintes des tables et modèles,
    profil numérique de référence
)

ObjectId = hash(WorldId, type, ancêtre structurel, chemin de subdivision)
TileKey  = (WorldId, couche, face, niveau, x, y)
~~~

Les paramètres sont sérialisés dans un ordre stable, avec unités explicites. Les constantes physiques et les distributions procédurales ont une version. Les identifiants ne dépendent ni des noms, ni du cache, ni de la position de la caméra.

Le relief final est défini par une hiérarchie finie, jusqu’au niveau métrique configuré. Le fait de ne pas avoir calculé une branche n’en fait pas un monde indéterminé : les entrées et les règles de cette branche sont déjà fixées. Les niveaux affichés sont des approximations de cette définition.

### Contrats obligatoires

| Invariant | Contrat |
|---|---|
| Permanence | Une unité recompilée sur le profil de référence produit les mêmes données canoniques |
| Réduction | Altitude et attributs se réduisent vers les moments du parent dans une tolérance annoncée |
| Relief | Le détail reste dans une enveloppe cumulée, spécifique à la géomorphologie |
| Côtes | Composantes importantes et connexions préservées ; déplacements dans des corridors |
| Drainage | Le quotient du réseau raffiné restitue les connexions et exutoires protégés du parent |
| Frontières | Même propriétaire, mêmes échantillons et mêmes ports des deux côtés |
| Eau | Production locale, stockage et entrées amont comptés une seule fois |
| Sédiments | Stocks et échanges de matière associés aux domaines, indépendamment des tuiles d’affichage |
| Objets | Une route ou une ville se précise ; sa connexion et son identité ne sont pas recréées |
| Cache | Une éviction ne modifie ni la graine, ni la qualité, ni les paramètres du solveur |

Les petits objets peuvent apparaître au zoom : un affluent, une île ou un village auparavant sous le seuil de visibilité. Leur existence est déterminée par la grammaire de subdivision. Leur introduction ne peut remplacer une connexion déjà protégée.

### Ce qui est garanti et ce qui est seulement visé

La reproductibilité, les interfaces et les bilans sont des exigences vérifiables. Le caractère convaincant du paysage et la plausibilité de sa géologie sont des critères à mesurer, pas des conséquences de ces seules exigences.

Le système ne prétend pas simuler toute la tectonique, l’atmosphère tridimensionnelle, la nappe et l’érosion d’une planète pendant des millions d’années. Il produit une **synthèse géomorphologique sous contraintes**, avec quelques sous-modèles physiques explicités.

## 5 Adressage sphérique et échantillonnage

### Cube sphérique explicite

Retenir six faces de cube et un quadtree par face. Pour éviter de rendre une projection complexe indispensable au premier prototype, utiliser la projection normalisée :

~~~text
u, v ∈ [-1, 1]
q = n_face + u e_u + v e_v
s = q / ||q||
position = (R + h) s
~~~

Les six bases orientées sont figées dans une table. L’inverse sélectionne la composante absolue dominante ; les égalités aux arêtes et sommets se départagent par un ordre fixe. Les voisinages changent d’orientation aux faces, pas de convention.

La projection n’est pas équivalente en aire. Calculer les aires et distances à partir de sa métrique sphérique ; ne jamais prendre une cellule pour un carré de côté constant. Une projection QSC exacte pourrait remplacer ce choix dans une nouvelle version, mais la documentation distingue bien QSC et son ancienne approximation COBE [R16][r16].

Dans les calculs : coordonnées et géométrie globale en float64, identités spatiales en entiers. Le rendu utilise des coordonnées relatives à une origine proche de la caméra.

### Résolutions

Une tuile comporte **256 × 256 cellules et 257 × 257 sommets**. Sur chaque face, le niveau z représente 256 × 2^z cellules par côté. L’échelle caractéristique est la racine de l’aire moyenne ; le pas réel varie selon la position.

| Niveau z | Pas caractéristique | Borne de pas sur les axes de grille pour R = 1 737 km |
|---|---:|---:|
| 0 | 9 819 m | 13 570 m |
| 2 | 2 455 m | 3 393 m |
| 4 | 614 m | 848 m |
| 6 | 153 m | 212 m |
| 8 | 38,4 m | 53,0 m |
| 10 | 9,59 m | 13,25 m |
| 12 | 2,40 m | 3,31 m |
| 13 | 1,20 m | 1,66 m |
| 14 | 0,60 m | 0,83 m |

La borne provient de 2R / (256 × 2^z). Elle ne borne pas la diagonale d’une cellule. Le niveau 14 convient au pas horizontal visé sur les axes ; un mesh plus dense peut être demandé pour un critère différent. Pour un autre rayon, recalculer le niveau terminal à partir de ce critère, sans réutiliser aveuglément z = 14.

### Séparer stockage et domaine de calcul

Une tuile est un **paquet d’affichage**, pas un bassin versant ni un domaine physique fermé. La génération minimale produit les quatre enfants d’un parent en une transaction : une famille de 512 × 512 cellules.

Le plan hydrologique peut couvrir plusieurs familles. Il est subdivisé en unités de versant et de tronçon avec des ports connus ; un domaine trop grand est partitionné par ces ports et ses contrats, puis résolu par blocs. Un halo sert au stencil numérique. Il ne remplace jamais la connaissance de l’amont.

## 6 Construction de l’atlas global

### Graphe de dépendances

~~~mermaid
flowchart TD
    A[Paramètres et graine] --> B[Géologie et relief initial]
    A --> C[Orbites et rayonnement]
    B --> D[Relief global et bassins]
    C --> E[Climat saisonnier]
    D --> E
    E --> F[Ruissellement et érosion globale]
    F --> D
    D --> G[Atlas et contrats figés]
    E --> G
    G --> H[Plans régionaux et raffinements]
    G --> I[Implantations et transports]
    I --> H
    H --> J[Tuiles certifiées]
    J --> K[Explorateur]
~~~

La boucle relief–climat–érosion n’existe que pendant la préparation, avec un nombre maximal d’itérations et des critères de convergence. Elle ne se réactive pas lorsqu’une caméra entre dans un bassin.

### Géologie génératrice

Créer des plaques sur la sphère par sites espacés et cellules de Voronoï. Chaque plaque possède une rotation d’Euler, un caractère continental ou océanique et un âge. Les vitesses relatives aux frontières donnent convergence, divergence et cisaillement. Elles génèrent champs de soulèvement, fosses, dorsales et failles, plutôt qu’une altitude finale obtenue uniquement par bruit.

Associer aux provinces des familles de matériaux : roches résistantes, couches tendres, formations volcaniques, dépôts alluviaux et surfaces anciennes. Stocker au minimum densité, érodabilité, perméabilité, cohésion, capacité de stockage et ressources. Les épaisseurs et contacts sont des champs ou surfaces ; les textures en sont une conséquence.

Prévoir des processus optionnels par province : cratères avec bourrelet et éjectas, karst de surface sans cavités volumétriques, formes glaciaires et aridité avec transport éolien simplifié. Leur activation dépend des paramètres et de l’histoire proposée. Le rayon ne décide pas du caractère lunaire du paysage.

L’histoire géologique est un ensemble de champs et de doses de processus synthétiques. On n’annonce pas une reconstruction chronologique réelle à partir de cette représentation.

### Relief global

1. Évaluer les champs à 256 cellules par côté de face.
2. Construire relief initial et relief sous-marin, puis déterminer le niveau marin.
3. Identifier les dépressions, les exutoires et les grands bassins.
4. Proposer un relief couplé au soulèvement et à l’incision.
5. Résoudre le climat et les budgets de ruissellement.
6. Recalculer relief et drainage jusqu’au critère ou à la limite du profil.
7. Augmenter la résolution par étapes pour la compilation finale.
8. Extraire les primitives persistantes, leurs profils et leurs enveloppes.

Retenir **l’érosion analytique de Tzathas et al. comme premier candidat pour le prototype**, et comparer avec un schéma implicite de type FastScape. La première évite le déroulement complet du temps géologique ; elle a néanmoins ses propres itérations de drainage et ses limitations de stabilité. Le second fournit un outil de comparaison pour incision, diffusion et dépôts [R6][r6], [R17][r17]. Aucun des deux n’est décrété meilleur avant mesure.

L’incision de référence peut s’écrire :

~~~text
∂h/∂t = U - K (A/A_ref)^m max(S - S_crit, 0)^n + diffusion + dépôt
~~~

U et K ont ici l’unité m/s grâce à la normalisation de l’aire A ; S est une pente sans dimension. Une version normalisée par débit peut être utilisée, mais ses paramètres sont distincts. Ne pas appliquer un coefficient calibré en années à un intégrateur en secondes.

### Deux profils opérationnels

| Paramètre | Prévisualisation | Compilation finale |
|---|---|---|
| Relief | 6 × 256² cellules, éventuellement 512² pour la sélection finale | 6 × 1 024² cellules |
| Climat réduit | 6 × 64² cellules | 6 × 128² cellules, 256² si nécessaire |
| Sorties temporelles | 12 phases de l’année, issues d’un intégrateur | Même définition, intégration et convergence plus poussées |
| Couplages relief et climat | Peu d’itérations, borne explicite | Contrôle de convergence et plafond de calcul |
| Objets humains | Potentiel et principaux pôles candidats | Quotas, pôles retenus, corridors et réservations |
| Objectif de durée | 1 à 3 minutes | 1 à 4 heures, paramétrable |

**Ces durées sont des cibles, pas des performances établies.** On réduit d’abord le nombre d’itérations ou la résolution de prévisualisation. On ne change pas le générateur canonique de détail en fonction du temps disponible.

Un atlas final de 6 291 456 cellules représente 24 Mio par champ float32, soit 384 Mio pour seize champs. Les graphes, états temporaires, saisons et index portent la consommation réelle à davantage ; le budget total du processus est défini en section 15.

### Publication

Avant publication, vérifier l’absence de cycles de drainage non modélisés, les connexions marines, la stabilité des bassins protégés et les résidus de budgets. Une compilation qui atteint sa limite sans satisfaire les seuils produit un rapport de diagnostic. Elle ne devient pas automatiquement une révision finale.

Une fois validé, l’atlas et son manifeste deviennent immuables. Toute modification de climat, de niveau marin, de géologie ou de population crée une nouvelle révision.

## 7 Raffinement proposé sous contrats

Cette section décrit la méthode nouvelle essentielle. Il faut en implémenter une preuve locale avant d’engager tout le système.

### Contenu d’un contrat

Chaque unité parent transmet :

- son domaine sphérique, sa métrique et une reconstruction de son altitude ;
- les moments d’altitude et de stocks de matière à conserver ;
- les bassins et objets protégés, avec leur application de parenté ;
- les profils des crêtes, lits et surfaces d’eau, ainsi que leurs corridors ;
- les ports : position, sens, altitude, débit saisonnier et échanges de sédiments ;
- le ruissellement produit localement, distinct des entrées amont ;
- les bornes de détail, de pente et de déplacement des frontières ;
- les réservations nécessaires aux sites, routes et ouvrages.

Les moments et les flux n’ont pas le même opérateur de réduction. Une moyenne arithmétique ne réduit ni une connexion, ni un débit traversant une frontière.

### Opérateurs de réduction

Le champ h est reconstruit sur un maillage triangulaire dont la subdivision est compatible avec le parent. Pour une cellule parent C, conserver d’abord son altitude moyenne surfacique :

~~~text
D_h(h_fine)[C] = (1 / aire(C)) ∫_C h_fine(x) dA
cible[C]       = (1 / aire(C)) ∫_C h_parent(x) dA
~~~

Les intégrales utilisent la métrique de la projection et une quadrature documentée. Des premiers moments et une erreur de reconstruction contrôlent également la dérive des formes longues. Conserver uniquement la moyenne pourrait masquer une bosse et une vallée artificielles.

Les ports se réduisent par **somme des flux orientés**. Les stocks se réduisent par intégration de masse ou de volume. Les catégories se réduisent par fractions surfaciques. Le graphe se réduit par contraction des éléments dont le parent est connu.

On ne définit pas D_h comme la simple lecture d’un sommet sur deux : cette restriction ne contrôle pas le volume ni les fréquences du relief. Les sommets structurants et les frontières ont leurs propres contraintes d’ancrage.

### Développer le plan hydrologique

Le plan d’un bassin est généré avant son raster de détail :

1. Conserver exutoires, confluences importantes et profils hérités.
2. Subdiviser les tronçons dans leurs corridors, selon une abscisse curviligne persistante.
3. Partitionner la production locale de ruissellement entre sous-bassins.
4. Ajouter des affluents candidats suivant pente, lithologie et distance aux crêtes.
5. Attribuer une relation parent à chaque nouvel objet et contrôler le quotient du réseau.
6. Construire un potentiel de drainage et un routage candidat vers les ports.
7. Vérifier la possibilité d’insérer ce plan dans les enveloppes du parent.

La partition est définie sur des domaines hydrologiques, puis découpée pour le calcul ; elle ne fait pas des côtés de tuiles des lignes de partage. Les tronçons qui traversent plusieurs tuiles possèdent un seul objet et un seul profil.

Une forme initiale combine profils de vallées, crêtes et versants. Une arborescence donne l’ordre amont–aval et un potentiel décroissant. Elle interdit les cycles, mais ne suffit pas à garantir que le terrain descend effectivement vers les rivières : le solveur contrôle aussi les altitudes.

### Frontières avec propriétaire unique

Attribuer un identifiant canonique à chaque sommet partagé, arête et portion de frontière. Le propriétaire est choisi par une règle totale sur les identifiants spatiaux. Il n’est jamais « la première tuile demandée ».

Une interface fournit :

- une trace d’altitude raffinable et des points d’ancrage ;
- un profil transversal dans un collier partagé ;
- les courbes et ports d’eau ou de routes qui la traversent ;
- une signature de parenté et les quantités intégrées à transmettre.

Le collier fixe aussi la première couronne intérieure nécessaire aux normales. Une largeur initiale de 8 cellules, plus un halo numérique de 32 cellules, est un paramètre à tester. Un opérateur nécessitant davantage de support ne peut supposer que 32 cellules suffisent.

**Ordre de construction :** plans parents voisins disponibles → interfaces fines et coins → plans enfants compatibles → surfaces intérieures. Les interfaces dépendent des parents, des objets canoniques et de leurs budgets, jamais du résultat intérieur d’un voisin au même niveau. Ainsi, il n’y a pas de dépendance circulaire entre deux tuiles à compiler.

Les intersections de plusieurs frontières sont construites d’abord comme un seul objet de coin. L’orientation aux faces du cube est traitée par la table de voisinage. Deux copies d’une même interface sont des vues des mêmes données.

Cette règle introduit un biais possible autour des frontières. Le collier doit se fondre dans l’intérieur avec une base de résidus qui s’annule en valeur et en dérivée au bord. Les tests doivent chercher des bandes rectilignes dans le relief et le drainage. Si elles restent visibles, il faut revoir les interfaces ou les domaines de génération ; les masquer par une texture ne valide pas la méthode.

### Proposer les formes fines

À chaque bande d’échelle :

1. Reconstruire le parent.
2. Ajouter un résidu déterministe orienté par géologie, crêtes, vallées et exposition.
3. Appliquer des propositions d’incision, de diffusion de versant, de transport et de dépôt.
4. Proposer côtes et sinuosités à l’intérieur de leurs corridors.
5. Transmettre le résultat candidat au solveur de contraintes.

Utiliser Schott et al. comme référence pour les effets multi-échelle, puis adapter les opérateurs aux unités et frontières du système [R7][r7]. Leur code officiel fonctionne sous Linux avec OpenGL compute ; le portage vers nos backends et contrats reste à réaliser [R18][r18].

Le budget de détail est associé à une longueur physique. Pour une classe de terrain, une première loi expérimentale est :

~~~text
A_z = A_0 (s_z / s_0)^H, avec 0 < H ≤ 1
E_z = somme des A_k pour les niveaux k > z jusqu’au niveau terminal
~~~

A_z borne l’amplitude du résidu ; s_z est son échelle. H n’est pas une constante universelle : roche, dunes, plaines et falaises reçoivent des lois différentes. Les grandes ruptures viennent de primitives dédiées, pas d’une amplitude de bruit sans limite. Les pentes ont des bornes distinctes, car une faible amplitude peut avoir une dérivée forte.

Le détail affine **un état géologique figé**. Il ne simule pas un nouveau million d’années d’érosion à chaque niveau. Les opérateurs produisent des propositions subgrille ; leur dose et leur bande sont versionnées.

### Résoudre les contraintes de surface

Pour un routage et des interfaces fixés, les altitudes aux sommets constituent un vecteur x. Le solveur recherche une surface proche de la proposition y :

~~~text
min_x  1/2 ||W(x - y)||²
     + λ_g ||G(x - U(parent))||²
     + λ_c ||L(x - U(parent))||²

sous :
    M x = moments_parent
    B x = interfaces_et_ancrages
    lit_amont >= lit_aval + marge
    borne_inf <= x <= borne_sup
    signe_terre_mer conforme à la côte choisie
    contraintes de drainage, de pente et de réservation
~~~

M intègre les fonctions de base ; B évalue des hauteurs ou traces. G et L sont des opérateurs de gradient et de courbure calculés avec la métrique. W et les coefficients sont normalisés pour éviter que changer de résolution change implicitement la fonction objectif.

Sur le terrain sec, imposer à chaque sommet un récepteur le long d’une arête du maillage :

~~~text
h_i - h_recepteur >= marge_i
~~~

La marge peut être nulle pour un plat traité explicitement ; un ordre symbolique départage alors le routage. Pour les récepteurs protégés, ajouter les inégalités qui empêchent un voisin concurrent de devenir une sortie descendante plus favorable vers un autre bassin. Elles sont linéaires lorsque les longueurs et le routage sont fixés. Le réseau et le terrain deviennent ainsi compatibles à la résolution certifiée.

Avec s_ij = (h_i - h_j) / d_ij, imposer s_i,r ≥ s_ij + ε_s pour un voisin j concurrent d’un récepteur r protégé. Les arêtes entre bassins sont également contrôlées par les courbes de partage et leur profil transversal. La certification porte sur ce drainage discret ; une simulation d’eau continue éventuelle doit être validée séparément.

Les contraintes de côte sont appliquées à un masque géométrique fixé pour la passe : altitude nulle sur la côte, signe correct de part et d’autre. Les étendues de lac utilisent leur propre cote d’eau. Faire dépendre le masque de x transformerait le problème en optimisation non convexe ; on sépare donc choix de géométrie et résolution.

**Implémentation proposée :** projection analytique des moments sans autres contraintes pour établir la référence, puis solveur primal–dual à opérateurs creux pour le problème complet. Les égalités de moments se projettent par blocs ; les bornes et inégalités ont des projections locales. Le solveur GPU fonctionne en float32 avec vérification des contraintes en float64. Les réductions utilisent des arbres fixes.

La projection simple sur M x = m s’écrit, pour une métrique Q définie positive et des contraintes M de rang plein :

~~~text
x_corrigé = x - Q⁻¹ Mᵀ (M Q⁻¹ Mᵀ)⁻¹ (M x - m)
~~~

Elle montre comment retirer les composantes qui changent les moments du parent. Elle ne prouve pas la faisabilité lorsque toutes les autres contraintes sont ajoutées. Le solveur complet, son préconditionnement et son coût font partie des points à valider.

### Matière et correction géométrique

Le solveur étendu porte aussi la roche B et l’épaisseur de sédiment T, avec h = B + T et T ≥ 0. Lorsque densités et porosités sont fixées dans une passe, les contraintes de stock solide sont linéaires. Les fractions de matériaux et leurs moments sont conservés séparément.

La conservation de l’altitude moyenne n’est pas équivalente à la conservation de masse lorsque densité ou porosité changent. **Ne pas assimiler la correction géométrique à de l’érosion physique.** Le résultat doit satisfaire les budgets de matière du contrat ou déclarer une incompatibilité. La simulation géologique globale peut avoir des sources et sorties ; elles sont enregistrées comme telles.

### Faisabilité et échec déterministe

Préserver moyenne, côte, pente, bassin et nouveau détail peut être impossible dans une région. La conception prévoit un ordre de priorité :

1. unités, identité, connexions et interfaces ;
2. bilans et moments dans leurs tolérances ;
3. bornes du parent ;
4. nouvelles branches et déplacement des courbes ;
5. ressemblance avec la proposition d’érosion.

Le planificateur essaie une liste **fixe et versionnée** de propositions : détail complet, amplitude réduite, affluent optionnel retiré, reconstruction héritée. Une proposition ne devient admissible qu’après contrôle des contraintes. L’ajustement ne dépend ni du temps écoulé, ni de la charge GPU, ni de l’ordre des demandes.

La reconstruction héritée est un témoin de faisabilité uniquement si les nouveaux contrats restent compatibles avec elle. Elle ne garantit pas qu’un nouveau fleuve, une nouvelle côte ou une nouvelle réservation soient réalisables. En absence de témoin, l’unité échoue et reste au parent affiché. Elle n’est pas publiée comme détail réussi.

Distinguer **une proposition géométriquement incompatible** d’**un solveur qui ne converge pas**. Un échec numérique ne doit pas déclencher arbitrairement une autre géométrie selon le matériel. Le profil de référence fixe les itérations, seuils et règles ; un diagnostic CPU peut analyser l’échec. Une correction des règles crée une nouvelle version.

### Ce que le certificat démontre

Chaque famille enregistre son erreur de réduction, ses marges de drainage, ses interfaces, son quotient de graphe et ses résidus de budgets. La conformité est vérifiée après quantification et encodage.

Sur un maillage linéaire, l’existence d’un chemin strictement descendant depuis chaque sommet sec vers un port, sans cycle, exclut les minima bloquants à ces sommets. Avec les contraintes sur les voisins et les labels de bassin, on contrôle les connexions de drainage à cette résolution. Les lacs et plats sont des cas explicites.

Ce certificat ne démontre pas une solution des équations de Navier–Stokes, ni l’absence de toute erreur géomorphologique. Il ne prouve pas non plus le succès futur de toutes les branches non calculées. La construction doit maintenir des témoins compatibles ; les essais de la section 18 vérifient ce point avant généralisation.




### Construction déterministe d’un plan candidat

Pour rendre le planificateur implémentable, partir d’un algorithme de référence simple, avant les variantes plus expressives :

1. Reconstructeur du parent et primitives héritées donnent une surface témoin h₀.
2. Échantillonner les domaines de bassin sur le maillage fin ; les lignes de partage et ports hérités sont des contraintes géométriques.
3. Construire les récepteurs candidats parmi les voisins accessibles sans montée sur h₀. Pour les plats déclarés, ajouter un ordre de distance vers la sortie, calculé par recherche multi-source à coût positif.
4. À l’intérieur d’un bassin, choisir un récepteur par coût de trajet combinant longueur, direction du versant et lithologie. Les distances et égalités se départagent par identifiant stable. Le potentiel strictement ordonné rend le graphe acyclique.
5. Allouer la production locale d’eau, accumuler dans l’ordre du graphe et sélectionner des branches selon aire ou débit de référence. Les tronçons protégés sont conservés indépendamment de ce seuil.
6. Simplifier les branches en polylignes, leur attribuer parenté et corridor, puis proposer sinuosité et incision.
7. Garder les déplacements de profil qui possèdent un témoin admissible. Les autres restent des propositions optionnelles, retirables dans l’ordre versionné.

Le coût ne doit pas permettre de franchir une ligne de partage protégée. Une recherche limitée aux pixels visibles est interdite. Si la reconstruction héritée ne possède pas de drainage admissible, l’erreur vient du contrat parent et doit être corrigée pendant sa compilation.

Pour une courbe héritée C₀(s), une première sinuosité est C(s) = C₀(s) + a(s) N(s), où N est la normale tangente sur la sphère. Les fonctions a s’annulent aux ancrages, possèdent une amplitude bornée et sont adressées par tronçon et intervalle dyadique de s. Le profil de lit utilise la même abscisse. Contrôler auto-intersections, orientation, corridor et monotonicité ; une sinuosité invalide n’est pas réparée par une nouvelle graine aléatoire.

### Construire une direction de détail admissible

Une alternative constructive au solveur général donne un premier générateur sûr. Si h₀ satisfait les égalités et A h₀ ≤ b, projeter un résidu r dans le noyau des égalités de moments et de frontières. Puis choisir :

~~~text
α = min(1, min_j [(b_j - A_j h₀) / (A_j r)])
                  pour les j où A_j r > 0
h = h₀ + α r
~~~

Cette borne garantit les inégalités linéaires **pour ce routage et cette géométrie fixés**. Elle donne aussi un témoin de faisabilité au solveur plus expressif. Quand α est trop petit, changer de direction admissible ou réduire une proposition optionnelle ; augmenter une tolérance en silence n’est pas autorisé.

Pour les interfaces raffinées, réserver une part de la marge parent à chaque arête et coin incident. Le propriétaire propose une perturbation avec compensation des moments dans les domaines adjacents, puis borne son amplitude par la marge des deux côtés. La proposition n’utilise pas leurs futures solutions intérieures. La somme des réserves ne doit pas dépasser la marge disponible.

Il faut vérifier que les témoins assemblés satisfont simultanément tous les colliers. Le cas où la perturbation d’interface est nulle fournit le repli de référence ; il ne garantit pas une frontière riche en détail. Le taux de directions bloquées et les motifs d’interface font donc partie du diagnostic de qualité.

### Exemple de conservation et de drainage

Dans un exemple à quatre échantillons d’aire égale, h₀ = [100, 99, 98, 97] m a une moyenne de 98,5 m. Le résidu [-1, 1, 0,5, -0,5] m conserve cette moyenne, mais appliqué entièrement il fait remonter le premier tronçon.

Avec une descente minimale de 0,2 m entre voisins, α ≤ 0,4 donne [99,6, 99,4, 98,2, 96,8] m : moyenne conservée et descentes de 0,2, 1,2 et 1,4 m. Dans le solveur réel, les poids sont les intégrales des fonctions de base et les distances sont sphériques.

Pour l’eau, un domaine recevant 50 m³/s, produisant localement 2 m³/s, perdant 0,3 m³/s et stockant 0,2 m³/s transmet 51,5 m³/s. Répartir sa production entre enfants en 0,8 et 1,2 m³/s ne leur donne jamais chacun une seconde copie des 50 m³/s entrants.

## 8 Rivières lacs et drainage saisonnier

### Un réseau global puis des sous réseaux persistants

Le graphe principal est construit pendant la préparation, depuis le relief et les budgets de ruissellement. Il contient les grands fleuves, les lacs, leurs seuils, les embouchures et les bassins fermés. Il porte également des chenaux intermittents : leur morphologie peut exister même quand leur débit instantané est nul.

Les détails suivent une grammaire par bassin. Une nouvelle branche reçoit une aire contributive, un quota de ruissellement, une lithologie et un profil de raccordement. Son insertion respecte la parenté, les lignes de partage et les ports. Les confluences importantes sont des ancrages ; les sinuosités se développent entre elles.

À une confluence, les profils de lit et d’eau se raccordent au même objet. Le profil longitudinal du lit est décroissant hors retenues et ruptures explicites. Pour un tronçon, interpoler un profil monotone ; ne pas perturber indépendamment ses altitudes avec du bruit.

Les estuaires et deltas utilisent un graphe orienté avec partage de flux. La somme des fractions sortantes vaut un. Un delta peut avoir plusieurs bras ; forcer tout réseau à être un arbre interdirait ce cas.

### Production d’eau et accumulation

Utiliser un modèle de sol à réservoirs simples, avec neige et réserve souterraine approximée. Pour une cellule ou un sous-bassin :

~~~text
dS_neige/dt = précipitation_solide - fonte - sublimation

dS_sol/dt = pluie + fonte - ET - recharge - ruissellement_rapide
dS_nappe/dt = recharge - débit_de_base - pertes_profondes

Q_sortie = somme(Q_entrées) + production_locale - pertes - dS_eau/dt
~~~

Toutes les quantités de la dernière ligne sont en m³/s. Les flux surfaciques en m/s sont multipliés par l’aire réelle. Les capacités de stockage viennent du sol et de la géologie. Les pertes profondes quittant le domaine doivent rejoindre un réservoir déclaré ou une sortie explicite.

À chaque subdivision, allouer le ruissellement parent R_p entre enfants selon des poids w_i :

~~~text
R_i = R_p × (aire_i × w_i) / somme_j(aire_j × w_j)
~~~

Cette identité conserve la production locale, mais elle ne suffit pas à conserver le bilan complet. Les propositions de pluie, ET et stockage sont donc projetées ensemble sur les budgets mensuels et les contraintes de réservoir : flux non négatifs, capacités respectées, continuité entre phases. Une allocation impossible est un échec de contrat.

Le débit amont est reçu par le port. Il n’est pas recalculé à partir de la pluie de la tuile. Une rivière allochthone peut traverser un désert, perdre une partie de son eau et conserver une vallée et une végétation riveraine. Aucun débit minimal fictif ne doit être ajouté sans provenance.

### Dépressions et lacs

Employer Priority-Flood pour analyser les dépressions et les connexions, sans décider que toutes doivent être supprimées [R8][r8]. Conserver une hiérarchie de dépressions et un fonctionnement de remplissage, débordement et fusion inspiré de Fill–Spill–Merge [R9][r9].

Un lac contient : cote de seuil, relation volume–niveau, aire inondée, apports, évaporation, infiltration et sortie éventuelle. La courbe hypsométrique principale est calculée avec l’atlas. Le détail local conserve des moments de volume et peut préciser le rivage dans une enveloppe.

Les bassins fermés persistent si apports et pertes ne permettent pas un débordement durable. La salinité est un attribut de stockage simplifié, utile au rendu et à l’écologie. Un lac salé n’est pas traité comme une portion d’océan.

Les variations saisonnières de niveau utilisent les états d’eau de la révision. Elles peuvent déplacer un rivage à l’intérieur de son enveloppe d’inondation ; elles ne déplacent pas le relief.

### Largeur profondeur et vitesse

Un débit ne donne pas à lui seul largeur et profondeur. Stocker une section hydraulique par famille de rivière et l’ajuster au débit, à la pente, à la rugosité, aux sédiments et au confinement de vallée. Une première solution est un écoulement uniforme de type Manning, avec coefficient de rugosité empirique :

~~~text
Q = (1/n_M) × aire_mouillée × rayon_hydraulique^(2/3) × pente_énergie^(1/2)
v = Q / aire_mouillée
~~~

Ce modèle approxime les tronçons ordinaires ; sa calibration n’est pas universelle, notamment si la gravité change. Ne pas supposer qu’une rivière en très faible pente doit accélérer parce que le lit a été artificiellement forcé à descendre.

Pour les mondes à gravité variable, préférer une fermeture de Darcy–Weisbach, qui expose directement g :

~~~text
v = sqrt(8 g rayon_hydraulique pente_énergie / facteur_friction)
Q = aire_mouillée × v
~~~

Le facteur de friction est sans dimension et dépend du nombre de Reynolds et de la rugosité relative ; viscosité et densité de l’eau dépendent de la température. La géométrie des berges borne les sections possibles. Si une phase dépasse la capacité du lit, la différence va vers stockage et inondation, pas vers une largeur de fleuve arbitrairement tronquée. Manning reste un outil de comparaison calibré sur les cas terrestres.

Pour les lacs, estuaires, zones de remous et confluences larges, conserver une surface d’eau compatible avec les niveaux aval. La préparation utilise un modèle de tronçon simplifié ; une simulation locale peu profonde peut enrichir une zone épinglée, avec conditions aux limites connues. Elle ne recalcule pas tout le bassin.

### Sédiments et forme du paysage

Chaque tronçon transporte un débit solide et des classes de matériau. L’érosion du lit alimente ce transport ; la capacité, les ruptures de pente et l’eau retenue contrôlent les dépôts. Les quantités sortantes sont transmises au domaine aval, y compris en mer.

Le terrain conserve roche exposée, épaisseur alluviale, granulométrie, humidité durable et fréquence d’inondation. Les terrasses, plaines alluviales et cônes sont générés comme primitives de bassin sous contraintes, puis amplifiés.

Un changement de niveau ne correspond pas à une nouvelle période de transport sédimentaire. Les stocks détaillés se réduisent vers ceux du parent. Les expériences locales de dynamique produisent des états temporels distincts, pas une mutation silencieuse de la révision.

## 9 Mer bathymétrie et littoral

La mer comprend un domaine connecté, un relief sous-marin, un niveau de référence, des côtes, des apports et des matériaux. Elle possède un rendu propre de l’espace au rivage. Elle ne se résume pas à colorer les altitudes négatives.

### Volume d’eau et niveau marin

Le paramètre de création principal est soit un volume d’eau disponible, soit une fraction de terres émergées souhaitée. Dans le deuxième mode, on déduit un volume compatible. On ne fixe pas simultanément volume, altitude marine et fraction continentale sans résoudre leur compatibilité.

Déterminer les océans par connectivité du relief sous-marin. Les dépressions terrestres isolées restent des réservoirs séparés. Pour un niveau η dans le domaine océanique :

~~~text
V_océan(η) = intégrale_océan max(η - h_fond, 0) dA
~~~

Une recherche monotone donne le niveau compatible avec le volume. Le domaine peut changer à l’ouverture d’un détroit : suivre les seuils et la hiérarchie de connexions. La cote moyenne validée est ensuite l’origine des altitudes de la révision.

Le système utilise une sphère comme référence gravitationnelle. Un géoïde et une déformation hydrostatique peuvent devenir des extensions versionnées. Il ne faut pas présenter la sphère de référence comme un calcul de ces effets.

### Relief sous marin

Les mêmes provinces tectoniques produisent plateaux continentaux, talus, plaines abyssales, dorsales, fosses et édifices volcaniques. L’épaisseur sédimentaire dépend de l’âge et des apports continentaux. Les canyons sous-marins peuvent prolonger les structures de versant et les exutoires.

Le raffinement bathymétrique suit les mêmes moments et interfaces que le relief émergé. Les lieux visités peuvent recevoir du détail métrique sous l’eau ; le fond océanique global n’est pas calculé à cette résolution.

Une loi d’amplitude distincte évite de mettre la même rugosité sur une plaine abyssale et une chaîne montagneuse.

### Côtes estuaires et deltas

L’atlas fournit les composantes de côte, les détroits et les corridors de déplacement. La subdivision ajoute caps, anses et îlots dans ces corridors. Elle contrôle signe de l’altitude, fractions de terre et connexions importantes.

Dans un relief très plat, une petite erreur verticale peut déplacer fortement le rivage. **La seule borne d’altitude est insuffisante.** Le contrat de côte inclut donc une enveloppe horizontale et des passages à préserver. Les détroits plus larges que le seuil protégé ne peuvent se fermer au zoom.

Les deltas sont des objets de dépôt liés au débit solide, au débit d’eau et à un paramètre d’exposition marine. Les estuaires ont un niveau aval marin et une zone de mélange. Les plages, marais côtiers, falaises et mangroves éventuelles dérivent du substrat, de l’énergie littorale, du climat et de l’inondation.

Le modèle distingue une côte de référence, une enveloppe intertidale et une zone de crue. Les variations d’eau affectent ces couches ; le calcul de détail ne déplace pas arbitrairement la côte de référence.

### Océan et climat

La première version emploie un océan à couche mélangée : capacité thermique plus grande que le sol, évaporation bornée par énergie et humidité, transport horizontal simplifié. Il participe au bilan atmosphérique et reçoit pluie, fleuves et fontes.

Les volumes saisonniers stockés sur les terres ont une contrepartie océanique dans le bilan global. Les détails régionaux ne créent pas une source d’eau additionnelle.

Un module optionnel calcule des courants barotropes moyens avec vent, Coriolis, friction et contrainte de transport sans flux à travers les continents. Il donne des courants de surface plausibles et un transport thermique approximé. C’est un calcul global grossier, pas une simulation tridimensionnelle de salinité, convection et turbulence. Il ne conditionne pas le premier résultat de relief et de navigation.

### Vagues marées et rendu

Les vagues visuelles utilisent vent, exposition et profondeur. La relation de dispersion :

~~~text
ω² = g k tanh(k d)
~~~

permet de faire dépendre la vitesse de phase de la gravité et de la profondeur. Un spectre de vagues grossier au large et des ondes locales proches du rivage suffisent initialement. Les directions se transportent dans les bases tangentes ; elles ne redémarrent pas avec une phase différente à chaque tuile.

Les marées sont une extension : potentiel gravitationnel des corps, réponse harmonique approximée par bassin et enveloppe intertidale. Additionner des hauteurs arbitraires pour plusieurs soleils ou satellites n’est pas un modèle de marée validé. Le mode initial peut afficher une mer calme à niveau moyen ; l’activation des marées a des paramètres et tests distincts.

Au rendu : absorption dépendant de la profondeur, diffusion simplifiée, reflets, écume localisée et turbidité près des apports. Les vagues ne déplacent pas les altitudes canoniques du fond. Depuis le globe, afficher la couverture et la couleur moyenne de l’eau ; depuis le sol, résoudre la surface et le rivage.

## 10 Climat planétaire et descente régionale

### Paramètres et domaines de validité

Entrées : rayon, gravité, rotation, obliquité, orbites, masse et spectre des étoiles, pression, composition atmosphérique, eau, albédo et flux géothermique. La masse implicite de la planète vaut g R² / G. Vérifier densité moyenne, vitesses, températures et domaine des approximations.

Un scénario compact d’essai peut utiliser R = 1 737 km et g = 3 m/s², correspondant à une densité moyenne d’environ 6 180 kg/m³. C’est une hypothèse de monde rocheux, pas une preuve de formation ou de maintien d’une atmosphère à long terme. Les tests de calibration utilisent aussi une planète de taille terrestre.

La pression, la gravité et la composition déterminent notamment masse de colonne, capacité thermique, échelle de hauteur et gradients thermiques. Les anciennes constantes « vent par latitude » et « moins 6,5 degrés par kilomètre » ne doivent pas être universelles.

### Rayonnement et plusieurs étoiles

Pour une surface de normale s à l’instant t :

~~~text
F(s,t) = somme_i [ L_i / (4 π d_i(t)²)
                   × max(0, s · direction_i(t))
                   × fraction_non_occultée_i(t) ]
~~~

Intégrer le spectre de chaque étoile avec les absorptions et albédos correspondants. Une température effective peut définir un corps noir approximé ; un spectre tabulé est préférable lorsqu’il existe.

Une étoile unique avec orbite képlérienne a une solution simple. Pour un système multiple, utiliser une éphéméride cohérente provenant d’une intégration gravitationnelle, par exemple REBOUND avec IAS15 et contrôle d’énergie [R19][r19]. Une liste de soleils fixes n’est pas suffisante pour produire des saisons orbitalement cohérentes.

Sous rotation rapide, intégrer le cycle diurne pour le climat moyen. Sous rotation lente ou synchrone, conserver la longitude et l’évolution des zones éclairées. Une moyenne journalière terrestre ne convient pas automatiquement.

Les douze phases de sortie sont une convention de visualisation pour une année de référence. Un système multiple non périodique reçoit une fenêtre temporelle et des états orbitaux explicites ; on ne répète pas artificiellement ses données tous les douze mois.

### Modèle réduit pour la première version

Proposer un modèle bidimensionnel de bilan énergétique et de transport d’humidité, calculé en volumes finis sur le cube sphérique. Pour la température effective de colonne et de surface :

~~~text
C ∂T/∂t = (1 - albédo) F
          - OLR(T, pression, composition)
          + divergence(D_T gradient(T))
          + transport_advectif_conservatif
          + flux_géothermique
~~~

C est en J/(m² K) ; les flux sont en W/m². Terre, eau et glace possèdent des capacités différentes. Le terme advectif et la diffusion redistribuent l’énergie ; leur intégrale doit s’annuler sur une planète fermée.

La fermeture thermique doit comptabiliser la chaleur latente. Dans une version à température effective unique, le terme local -L_v(E - P) et le transport de l’énergie de vapeur complètent cette équation ; l’énergie totale comprend C T + L_v q_col. Pour l’atmosphère, q_col est en kg/m² et E, P en kg/(m² s). Les flux d’eau de l’hydrologie sont convertis avec la densité. Un bilan radiatif correct ne compense pas une source de chaleur latente oubliée.

Pour commencer, OLR peut être une fermeture radiative grise bornée :

~~~text
OLR ≈ σ T⁴ / (1 + 3 τ_IR / 4)
τ_IR ≈ somme_X κ_X × fraction_massique_X × pression / g
~~~

Les coefficients d’absorption ont ici l’unité m²/kg. Cette fermeture est une approximation proposée, insuffisante pour tous les mélanges gazeux et régimes de température. Une table issue de calculs radiatifs ou de cas ExoPlaSim pourra la remplacer. L’interface radiative est séparée du terrain.

### Vents et pluie

Le bilan énergétique seul ne fournit pas les vents. Prévoir deux modèles explicitement séparés :

- **Prévisualisation :** circulation prescrite avec échelle de cellules dépendant de la rotation et du rayon, corrigée par continents et gradients thermiques. Elle est indicative.
- **Compilation de référence :** modèle réduit de couche atmosphérique tournante avec relaxation thermique et transport conservatif d’eau.

Une formulation candidate pour le second est :

~~~text
∂u/∂t + (u · ∇)u + f s×u = -g' ∇η - u/τ_drag
∂η/∂t + divergence((H_eq + η)u) = (η_cible(T) - η)/τ_rad
∂q_col/∂t + divergence(q_col u) = évaporation - précipitation
f = 2 Ω sin(latitude)
~~~

η est une épaisseur de mode équivalent, pas une altitude réelle de l’atmosphère ; g' et les temps de relaxation sont des paramètres de modèle. Cette fermeture est une **proposition de réduction**, pas un GCM validé. Elle évite une division géostrophique singulière à l’équateur, mais ne reproduit pas toute la convection, les jets et les tempêtes.

Employer un schéma conservatif avec limiteur et contrôle CFL. Fixer la convergence par résidus et variation des moyennes de cycle, pas par « dix itérations suffisent ». Tester les jets et transports sur des cas simples puis contre ExoPlaSim.

La condensation est limitée par la quantité de vapeur, la saturation et l’énergie disponible. Les montagnes ajoutent soulèvement, délai de condensation et transport des condensats. Les modèles linéaires d’orographie fournissent une référence pour éviter une pluie instantanément proportionnelle à toute pente positive [R20][r20].

### Descente d’échelle sans refaire le climat

Le raffinement régional utilise températures, vents et humidité de l’atlas, avec :

- gradient thermique dépendant de g, de l’humidité et de la capacité calorifique ;
- pression locale hydrostatique ;
- exposition solaire et ombrage topographique ;
- transport orographique de l’humidité sur un support régional déclaré ;
- sol, neige, stockage et redistribution de pluie.

La redistribution conserve la pluie et les composantes du bilan à l’échelle du parent. Elle représente une fermeture subgrille. Si une nouvelle montagne aurait changé toute la circulation globale, elle dépasse l’enveloppe autorisée du parent et demande une nouvelle compilation, pas une correction locale.

Calculer les effets orographiques régionaux sur une grille d’environ 150 à 600 m, selon le profil, avec longueur de support liée au vent et aux délais de condensation. La pluie n’est pas advectée sur toutes les cellules métriques. Au mètre, interpoler ces champs et détailler l’humidité du sol selon substrat et accès à l’eau. Cette séparation évite de demander des kilomètres de voisins métriques pour calculer un seul rivage.

La météo de rendu peut ajouter nuages, variations de vent et précipitations visuelles à partir d’un WeatherId. Elle ne réécrit ni le drainage de référence, ni les villes.

### Validation physique facultative mais prévue

ExoPlaSim sert d’outil hors ligne : comparer températures, gradients, saisons, précipitations et vents pour une petite bibliothèque de paramètres [R13][r13], [R21][r21]. Il utilise notamment des outils Fortran et peut exploiter MPI ; son installation et ses calculs sont isolés du serveur interactif.

Ne pas promettre qu’une série de simulations GCM tient dans les quelques heures accordées à chaque monde. Elles servent à calibrer des familles de paramètres, avec réemploi des résultats. Un monde exotique hors de ces familles est marqué comme extrapolation.

La documentation consultée présente le support de plusieurs sources lumineuses comme une évolution prévue. Il faut vérifier la version effectivement installée avant de supposer ce support natif [R21][r21]. Notre interface d’éphémérides reste indépendante.

## 11 Biomes sols et végétation

Les biomes sont une couche écologique du monde et un outil de rendu. Ils suivent relief, climat et eau ; ils influencent en retour les paramètres d’érodabilité et de sol pendant la compilation. Ils restent secondaires à la réussite du relief, des rivières et de la mer.

### Variables et classification

Calculer des variables continues : énergie annuelle utile, température minimale, amplitude saisonnière, durée du gel, déficit hydrique, réserve du sol, fréquence d’inondation, salinité et exposition. Une classification discrète dérive de ces variables pour l’interface.

Une première bibliothèque terrestre distingue glace, toundra, prairie ou steppe, forêt tempérée, forêt humide, désert, marais et végétation côtière. Elle est paramétrée, pas plaquée comme vérité sur une planète à chimie différente. Un monde non compatible avec cette écologie conserve des matériaux minéraux et des classes climatiques.

Le biome dominant ne suffit pas pour la tuile. Conserver des fractions et des transitions, ainsi qu’un potentiel de couverture végétale. L’altitude et l’humidité varient localement ; les limites ne doivent pas faire des bandes de couleur arbitraires.

### Hydrologie visible

La végétation riveraine utilise accès à l’eau, fréquence de crue, salinité et saison. Une vallée alimentée par un fleuve conserve son contraste dans une région sèche. Un chenal intermittent reçoit des matériaux et traces d’écoulement même en saison sèche.

Alluvions, zones humides, dunes, affleurements et neige sont des attributs matériels séparés du nom de biome. Ils peuvent se combiner dans une même région.

Les fractions se réduisent par aire vers le parent. La surface totale d’une forêt ne doit pas quadrupler parce que quatre enfants ont été classés indépendamment.

### Placement des instances

Générer arbres, buissons et rochers depuis une distribution spatiale hiérarchique à identifiants stables. Chaque candidat appartient à une cellule de placement canonique, avec position et attributs issus de son hash. Les filtres écologiques décident de son existence.

Utiliser des niveaux imbriqués de densité pour le rendu, des billboards à distance et des meshes près de la caméra. Les cellules de placement sont indépendantes des tuiles de rendu. L’apparition d’arbres au zoom ne change pas le terrain ou le bilan d’eau.


## 12 Implantations ressources et routes

Cette couche arrive après validation du relief et de l’hydrologie. Elle décrit une géographie humaine de référence, sans simuler automatiquement des siècles de politique.

### Hypothèses de civilisation

Le profil d’essai est une société agraire disposant de routes, de ponts simples et de navigation. Ce choix est une hypothèse configurable : technologie, population, régime d’échanges, utilisation du fleuve, tolérance aux risques et capacité d’irrigation appartiennent au manifeste.

Changer ces hypothèses crée une révision de la couche humaine ; le monde physique peut rester identique si son identifiant est séparé. Un paramètre de technologie ne doit pas modifier discrètement la tectonique.

### Pôles et population

Définir un potentiel de site à partir d’eau durable, productivité du sol, ressources accessibles, risques de crue, accès maritime ou fluvial et coûts de transport. Les ressources minérales proviennent de provinces géologiques ; une carte de minerai indépendante du substrat serait incohérente.

Proposer des candidats espacés sur la sphère. Pour chaque bassin ou province économique, allouer un budget de population et de production. Choisir les pôles par optimisation avec distance minimale, accès à l’eau et capacité locale. Une pondération illustrative est :

~~~text
score_site = bénéfice_eau + production_agricole + ressources + accès
             - risque - coût_aménagement - isolement
~~~

Les termes sont normalisés selon le profil de société, pas additionnés dans des unités incompatibles.

Pendant la préparation, retenir les centres principaux et préciser leurs sites dans des régions calculées vers 40 à 10 m. Réserver un polygone de placement avec pente, eau et aléas admissibles. Le point exact ne dépend ensuite plus de la première visite.

Les villages secondaires utilisent les quotas restants, une subdivision spatiale déterministe et des budgets hérités. Un village révélé localement ne reçoit pas une population inventée en supplément du total global.

### Réseau de transports

Construire d’abord un graphe d’échanges entre pôles : proximité, volumes de production et complémentarité. Sélectionner un réseau connecté avec quelques redondances utiles. Les connexions importantes possèdent des identifiants et des corridors persistants.

Le coût de chemin combine distance géodésique, pente longitudinale, pente transversale, substrat, franchissements, terrassement et maintenance :

~~~text
coût = intégrale_tracé coût_local(s) ds + somme(coûts_ouvrages)
~~~

Les pentes peuvent être directionnelles ; un chemin de transport n’est pas nécessairement symétrique. Les contraintes de courbure dépendent du mode de déplacement. Utiliser A* ou une recherche équivalente sur un graphe métrique, puis lisser avec contrôle de pente. Les méthodes de Galin et al. motivent ce routage par coût [R14][r14].

La voie navigable est un autre type d’arête, conditionné par profondeur, saison, pente, retenues et technologie. Un fleuve ne devient pas automatiquement navigable sur toute sa longueur.

### Faisabilité au zoom

Un corridor grossier ne prouve pas qu’une route métrique sera constructible. Avant publication, contrôler les passages critiques à une résolution intermédiaire et construire une réservation qui borne les perturbations futures du terrain : pente admissible, clearance d’ouvrage et volumes maximaux de déblais ou remblais.

Si ces bornes sont trop conservatrices, préparer les tronçons critiques plus finement. Si aucun tracé admissible ne subsiste, choisir une alternative pendant la compilation ou déclarer la connexion absente. Ne pas déplacer après publication un pont pour réparer une gorge révélée au zoom.

La plateforme, les piles de pont et les terrassements sont une couche d’aménagement définie sur le terrain naturel. Le relief naturel et la surface construite restent inspectables. Les formes fines des bâtiments et quartiers respectent les réservations, les accès et les budgets de surface.

## 13 Visualisation du globe au sol

### Rendu indépendant du générateur

Retenir un visualiseur Rust, compilé en WebAssembly pour le navigateur, avec wgpu et des shaders WGSL. Le même module de rendu peut avoir une enveloppe native pour les benchmarks et le diagnostic. wgpu prend en charge notamment Vulkan et DirectX 12 en natif, ainsi que WebGPU et WebGL2 dans le navigateur [R22][r22].

Le chemin navigateur principal utilise WebGPU. Un repli WebGL2 est une option de rendu à tester séparément ; il ne peut pas exécuter les compute shaders du compilateur. Le navigateur ne reçoit jamais le graphe global ni le solveur de génération. Un petit chargeur HTML/JavaScript suffit à démarrer le module WebAssembly.

Le calcul lourd est local et natif dans les deux modes. « Web » décrit l’interface d’exploration, pas une génération planétaire exécutée dans WebAssembly. Le service peut fonctionner sans connexion Internet après installation des dépendances et assets.

Partager caméra, sélection LOD, matériaux, meshes et gestion des objets dans un crate de rendu. Le frontend web prend les données via le service local ; le frontend natif utilise une interface TileSource pouvant lire directement les paquets et communiquer avec le worker. Une enveloppe winit et, si utile, une interface egui constituent des choix Rust pour le mode natif et les contrôles [R30][r30].

Le mode natif peut réduire copies, décodage et coût d’interface. Il ne gagne pas automatiquement sur les shaders, la compilation du monde ou les dépendances froides. Un partage direct de buffers GPU est une optimisation ultérieure, qualifiée par backend ; il n’est pas requis pour la validité du système.

Utiliser le même cube sphérique que le compilateur. Chaque patch possède un mesh régulier, une carte d’altitudes et ses attributs. Les index peuvent être réutilisés. Ne pas reconstruire un maillage irrégulier global à chaque mouvement.

Une variante avec clipmaps proches du sol peut être évaluée plus tard. Elle ne doit pas créer un deuxième modèle de terrain. Les geometry clipmaps constituent une référence pour les grilles régulières et les transitions [R15][r15].

### Sélection des niveaux

Déterminer le niveau par erreur projetée :

~~~text
erreur_pixels ≈ erreur_géométrique_m
                 × hauteur_écran_pixels
                 / (2 × distance_m × tan(FOV_vertical / 2))
~~~

L’erreur géométrique comprend les bornes cumulées du détail non affiché et l’erreur de maillage. Une erreur mesurée seulement sur les enfants déjà calculés serait trop optimiste ailleurs.

Prévoir culling par horizon, frustum et enveloppe d’altitude. Les voisins affichés diffèrent au plus d’un niveau, y compris aux arêtes de cube. Le parent reste visible tant que la couverture nécessaire des enfants n’est pas prête.

Une transition morphée sur 0,2 à 0,5 s interpole vers les enfants. Les transitions de bord utilisent le profil du niveau voisin. Les skirts sont une protection d’affichage secondaire, pas une réparation des données.

### Précision

La caméra et les centres de tuile sont en float64 côté CPU. Le GPU reçoit positions locales et origine haute/basse précision si nécessaire. L’altitude est ajoutée à la direction sphérique dans un référentiel proche de la caméra.

Séparer les passes lointaines et proches ou utiliser une stratégie de profondeur adaptée, afin que globe, rive et route ne scintillent pas ensemble. Tester l’interaction avec eau et objets transparents.

Le picking retourne coordonnées géographiques, WorldId, couche, objet et résolution disponible. Une altitude interrogée au parent est annoncée avec sa résolution ; demander une valeur canonique terminale peut déclencher un calcul asynchrone.

### Matériaux et échelles visuelles

L’apparence dérive de roche, sédiments, sol, humidité, inondation, exposition, climat et couverture végétale. Les textures sont un atlas de matériaux réutilisables, avec variation procédurale déterministe ; il n’y a pas une immense image satellite par planète.

Les motifs doivent être filtrés selon leur fréquence projetée. Au globe, afficher albédo moyen, neige, grands cours d’eau et masses végétales. À l’échelle régionale, révéler alluvions, versants et réseaux secondaires. Au sol, révéler rugosité, pierres, végétation et objets.

Les normales utilisent les échantillons et colliers canoniques. Leur continuité et l’éclairage sont testés séparément de la continuité d’altitude.

### Eau et atmosphère

Les fleuves sont des surfaces continues issues des objets de tronçon, avec largeur, niveau, vitesse et turbidité. Les rubans raccordent leurs confluences ; la largeur physique peut devenir subpixel au globe. Une couche cartographique peut grossir les tracés pour la lecture, en restant distincte du rendu physique.

Les lacs ont un niveau partagé, et la mer une surface sphérique suivant son niveau de référence. Pour éviter de faire une géométrie océanique métrique globale, rendre l’eau selon la profondeur et les courbes de rivage, puis ajouter le détail de vague local.

La lumière et l’atmosphère utilisent rayon, profils de densité et spectres. L’implémentation de Bruneton est une référence pour la diffusion paramétrable et les unités, y compris hors cas terrestre [R23][r23]. Les paramètres d’atmosphère du rendu et du climat proviennent du même manifeste.

### Interaction et streaming

Prévoir rotation du globe, zoom continu, déplacement tangent, vue oblique et mode proche du sol. Montrer la résolution actuellement disponible lorsqu’un raffinement est attendu. Une première visite lente doit rester navigable.

Le client priorise le centre d’écran et la trajectoire probable. Les requêtes portent un identifiant de vue ; celles devenues inutiles sont retirées de la demande. La compilation partagée peut continuer si d’autres consommateurs ou une région épinglée l’attendent.

En télémétrie, distinguer FPS, temps de téléchargement, temps de compilation, attente de dépendances et encodage. Un FPS élevé avec des minutes d’attente de relief n’est pas une exploration satisfaisante.

## 14 Architecture logicielle données et API

### Composants retenus

| Composant | Technologie proposée | Responsabilité |
|---|---|---|
| Noyau canonique | Rust | Identités, métrique, plans, graphes, contrats, contrôles et stockage |
| Calcul de champs portable | Rust avec wgpu et shaders WGSL | Stencils, propositions d’érosion et résolution de surface |
| Calcul CUDA pour WSL et NVIDIA | Rust avec cudarc et petits kernels CUDA | Même contrat de calcul lorsque Vulkan n’est pas utilisable ou CUDA est plus efficace |
| Compilateur et analyses canoniques | Rust, CLI et bibliothèque partagées | Préparation globale, profils et contrôles |
| Service local | Rust avec Axum et Tokio | Planification des demandes et transport |
| Explorateur | Rust, WebAssembly et wgpu | Caméra, LOD, streaming, matériaux et objets |
| Recherche et ML facultatifs | Outils externes isolés, dont Python si nécessaire | Comparaisons GCM et entraînement hors production |
| Index transactionnel | SQLite | Manifestes locaux, objets de cache, jobs et références |
| Charges binaires | Blocs compressés Zstandard | Atlas, familles et attributs, sans JSON massif |

Rust ne doit pas recalculer un stencil d’érosion par pixel sur le CPU lorsqu’un noyau GPU existe. Inversement, un graphe topologique avec dépendances irrégulières peut rester sur CPU tant qu’il ne domine pas le temps. Le GPU ne constitue pas une obligation pour chaque fonction. Axum s’intègre à Tokio pour le service asynchrone [R28][r28] ; les calculs CPU longs utilisent un pool dédié.

Un worker Rust garde le contexte du backend GPU, les buffers et le manifeste actif. CLI et service partagent la même bibliothèque du monde. Les échanges utilisent tableaux contigus, descripteurs de blocs et poignées de jobs, sans transit des cartes par des listes JSON ou par Python.

Séparer les tableaux par attribut, utiliser des indices compacts pour les graphes locaux et éviter les allocations par pixel. Les voisinages de l’atlas sont implicites lorsque possible. Garder les buffers GPU résidents entre les passes ; le retour CPU sert au certificat et à la publication, pas à chaque itération.

Les shaders WGSL travaillent en float32 ; la métrique et les contrôles critiques restent en float64 dans Rust. Fixer tailles de workgroup, réductions et politiques de quantification dans le profil.

L’interface ComputeBackend expose allocation, lancement d’opérateur, synchronisation, lecture et métriques. La géométrie, les plans et certificats restent communs. Deux implémentations sont prévues : wgpu et cudarc. Ce dernier permet de piloter CUDA et la compilation NVRTC depuis Rust [R29][r29]. Les kernels GPU CUDA sont un module de compatibilité, pas un noyau applicatif C++ ni un serveur Python.

Les opérateurs des deux backends ont une spécification commune d’unités, entrées, sorties et réductions. Comparer chaque opérateur et une famille complète à la référence CPU avant qualification. Une révision fixe son backend canonique ; changer le client web pour le client natif ne change pas ce backend.

### Arborescence future

L’implémentation pourra commencer dans un répertoire distinct, par exemple **next/**, pour conserver les prototypes comme références :

~~~text
next/
  core/             identités, sphère, graphes, contrats et certificats
  compute/          pilotage Rust, backends wgpu/CUDA et références CPU
  compiler/         préparation globale, profils et outils Rust
  service/          API et ordonnanceur
  viewer/           explorateur Rust avec cibles native et WebAssembly
  experiments/      preuves locales et jeux de comparaison
  schemas/          formats et manifestes versionnés

runtime/worlds/<WorldId>/
  manifest.json
  atlas/
  plans/
  certificates/
  packages/
  index.sqlite
  reports/
~~~

Cette arborescence est une proposition. Aucun répertoire d’implémentation n’est créé par le présent travail.

### Manifeste et objets

Le manifeste contient au minimum :

~~~text
WorldManifest
  identity, algorithm_versions, numeric_reference, source_licenses
  radius_m, gravity_m_s2, rotation_s, atmosphere
  orbital_system, epoch, climate_window
  water_inventory, sea_reference, geology_history
  civilization_profile, population_budget
  atlas_resolution, terminal_step_m
  solver_parameters, tolerances, detail_envelopes
  persistent_objects_hash, compilation_report_hash
~~~

Distinguer **PhysicalWorldId**, **HumanLayerId** et **WeatherId** permet de changer une société ou un instant sans recopier tout le monde physique. Le WorldId de publication référence ces composants.

Une famille contient ses enfants, les contrats utilisés, un index de leurs objets, les données canoniques et un certificat. Le certificat décrit notamment les erreurs après décodage, pas seulement les buffers temporaires.

### Format de tuile

Commencer avec des altitudes canoniques en **entiers de centimètres sur 32 bits**. Il s’agit d’une quantification de stockage, distincte de la précision physique. Une bordure est décodée depuis les mêmes valeurs canoniques chez ses voisins.

Budget indicatif de charge d’affichage, proche de 24 octets par sommet : altitude, normale encodée, épaisseur de dépôt, humidité, mélanges de matériaux, attribut d’écoulement, distance côtière et masque ou profondeur d’eau. Les ports, identités et courbes sont des objets séparés.

Une tuile à 257² sommets et 24 octets par sommet vaut environ **1,51 Mio sans compression**. Quatre enfants correspondent à environ 6 Mio. Les couches saisonnières sont demandées séparément. La compression réduit éventuellement la taille ; aucune réduction particulière n’est supposée dans les budgets.

Éviter les offsets et échelles de quantification indépendants qui décodent différemment une frontière commune. Une compression plus agressive doit passer les mêmes contrôles de géométrie et de drainage.

### Persistance et immutabilité

Écrire une famille dans un paquet temporaire, calculer ses empreintes et son certificat, puis publier sa référence SQLite dans une transaction. Une interruption avant publication ne laisse pas de famille partiellement valide.

Utiliser des paquets regroupés plutôt qu’un fichier par petit objet. Une taille initiale de 64 à 256 Mio est un choix d’ingénierie à mesurer. Prévoir compactage, contrôle d’intégrité et récupération après arrêt.

L’atlas et les objets structurants sont persistants. Les détails sont régénérables, sauf régions épinglées. La fermeture des dépendances de ces régions, leurs contrats et leurs versions doit être conservée.

### API proposée

Les chemins suivants sont des interfaces à créer, pas des commandes déjà disponibles :

~~~text
POST   /worlds/preview                 paramètres → job de prévisualisation
POST   /worlds/compile                 paramètres retenus → job de préparation
GET    /jobs/{id}                      état, étapes, mémoire, rapport
GET    /worlds/{id}/manifest           contrat de publication
POST   /worlds/{id}/requests           liste bornée de tuiles et couches
GET    /worlds/{id}/tiles/{key}        charge binaire validée ou état d’attente
GET    /worlds/{id}/objects/{id}       géométrie et attributs persistants
POST   /worlds/{id}/regions/pin        emprise et résolution à conserver
DELETE /requests/{id}                 abandon du besoin de la vue
GET    /worlds/{id}/reports            diagnostics et métriques
~~~

Une requête de vue obtient une version explicite. Le service refuse une tuile d’une autre révision et n’expose pas de données non certifiées comme définitives.

### Ordonnanceur

États : absent, dépendances attendues, planifié, calcul, vérification, publié, échec. Une même clé possède un seul travail actif ; plusieurs clients s’y abonnent.

La priorité modifie l’ordre, jamais le résultat. La fermeture de dépendances contient ancêtres, interfaces, coins et plans hydrologiques nécessaires. Elle est construite avant le lancement, avec détection des cycles et bornes de file.

Ne jamais ajouter une dépendance réciproque à un voisin fin pour « recoudre » un résultat déjà calculé. Une frontière appartient à son contrat, pas aux deux résultats qu’elle doit raccorder.


## 15 Budgets de ressources et objectifs de performance

### Budgets de fonctionnement

Les quotas suivants sont des valeurs initiales pour la machine observée. Leur application doit être réelle dans l’ordonnanceur, pas une annotation de configuration.

| Ressource | Budget initial | Mécanisme |
|---|---:|---|
| RAM du compilateur et worker | 12 Gio au total | Tableaux bornés, blocs et éviction |
| RAM du service | 1 Gio | Files limitées et références vers le worker |
| VRAM du calcul | 8 Gio | Buffers réutilisés et nombre de jobs actifs limité |
| VRAM du viewer | Cible de 2 Gio | Textures et meshes sous budget |
| Threads CPU de fond | 6 à 8 | Garder de la disponibilité pour la navigation |
| Disque atlas et plans | 10 Go | Formats compacts et prévision avant publication |
| Disque régions épinglées | 20 Go | Quota explicite, pas d’éviction automatique |
| Disque cache régénérable | 30 Go | Éviction pondérée par coût et réutilisation |
| Données et modèles optionnels | 20 Go | Corpus local limité |

Le plafond disque total proposé est donc **80 Go**, laissant une marge sur les 150 Go libres observés. Les fichiers de compilation temporaires doivent entrer dans ce plafond ou dans une réserve explicite. Une région épinglée trop grande nécessite un changement de quota, pas une croissance sans limite.

Les 150 Go ont été observés sur le volume Linux. Mesurer séparément l’espace disponible si le runtime est placé sur un volume Windows. Le quota s’applique à l’ensemble des révisions conservées, pas à 80 Go supplémentaires par monde ou par backend.

La VRAM déjà utilisée par d’autres applications n’est pas disponible au compilateur. Le service lit la mémoire libre et limite ses travaux. L’adaptation de charge change la concurrence et les pauses, jamais les paramètres canoniques de génération. Ne pas arrêter les processus d’un autre projet.

Une famille de 512² cellules avec 64 octets d’état par cellule représente 16 Mio ; halos, doubles buffers, opérateurs et variables du solveur augmentent cette taille. Commencer avec une enveloppe mesurée de 128 à 256 Mio par famille active. Les opérateurs sont appliqués sans matérialiser une matrice dense.

### Cibles à mesurer

| Mesure | Cible initiale | Qualification |
|---|---|---|
| Prévisualisation | 1 à 3 min | Monde réduit, qualité indicative |
| Préparation finale | 1 à 4 h | Atlas, contrôles et régions choisies |
| Navigation | 60 images/s à 1080p | Parents visibles pendant les calculs |
| Tuile chaude | p95 inférieur à 100 ms | Service local, lecture et décodage compris |
| Famille fine avec dépendances présentes | p95 inférieur à 1 s | Cible du noyau optimisé, pas résultat annoncé |
| Première zone métrique froide | Cible de 10 à 60 s | Fermeture de dépendances et difficulté incluses |
| Demandes actives | 128 familles en attente, 2 à 4 en calcul au départ | État logique plus grand paginé si nécessaire |

Le temps froid peut dépasser cette cible dans une région difficile. Le rapport de benchmark doit le montrer. On ne mesure pas uniquement un noyau simple avec ses données déjà sur GPU.

Au rendu, commencer avec un plafond de **2 à 4 millions de triangles**, puis ajuster. Une texture d’altitude à 257² n’impose pas 257² sommets pour chaque patch lointain : utiliser des meshes 65² ou 129² lorsque leur erreur projetée est acceptable, et la densité terminale près du sol.

Les noyaux de calcul sont découpés en lots courts afin de limiter les à-coups du navigateur qui partage la 3090. Les files GPU ne constituent pas une garantie d’isolement du rendu ; mesurer les temps de frame avec préparation et exploration simultanées.

### Cache et coût de première visite

Conserver en mémoire les niveaux parents et interfaces les plus utiles. Épingler avant exploration les départs, régions exemplaires, pôles et passages critiques. Un survol rapide demande surtout les niveaux intermédiaires, pas toutes les tuiles métriques sous la trajectoire.

À coût de régénération élevé, conserver les plans et contrats avant les textures d’affichage. Le remplacement de cache associe taille, fréquence d’utilisation et coût mesuré de reconstruction.

La fermeture d’une demande peut remonter jusqu’à l’atlas et toucher des plans voisins. Elle doit rester finie et documentée. Mesurer sa taille dans les essais froids ; si elle grossit avec toute la surface du monde au lieu d’une hiérarchie locale, l’architecture de plans doit être revue.

## 16 Déploiement local et conduite des calculs

### Environnement

Installer le runtime sur le système de fichiers natif de l’OS qui exécute le worker GPU. Les sources de comparaison peuvent rester ailleurs ; ne pas copier les anciens environnements ou dépendances Windows dans le nouveau système.

Le compilateur et le service sont des binaires Rust de release partageant un Cargo.lock et une version de toolchain. Isoler les environnements du climat externe et du ML. Verrouiller les versions après un premier cas reproductible. La présence de CUDA ne prouve pas que wgpu peut utiliser la 3090 dans le même environnement.

Compiler et profiler en release. Le workspace sépare les crates compatibles WebAssembly du compilateur natif et des backends GPU de calcul. Les jobs longs ne s’exécutent ni dans une callback de rendu, ni sur les threads async de requêtes. Garder le nombre de threads contrôlé entre les pools CPU, le service et les éventuels outils externes.

La validation Rust comprend formatage, analyse statique, tests des invariants et benchmarks reproductibles. Les gains de Rust se mesurent sur allocations, disposition des données, dépendances et transferts ; le choix du langage ne supprime pas les coûts de l’algorithme.

### Déploiements Windows et Linux

| Environnement | Compilateur et service | Visualiseur |
|---|---|---|
| Windows natif | Rust avec wgpu/DirectX 12 ; CUDA possible après qualification | Navigateur Windows en WebGPU ou client Rust natif en DirectX 12 |
| Linux natif avec driver NVIDIA | Rust avec wgpu/Vulkan ou cudarc/CUDA | Navigateur WebGPU qualifié ou client Rust natif Vulkan |
| WSL de la machine actuelle | Rust avec cudarc/CUDA comme premier chemin à tester ; wgpu/Vulkan après qualification | Navigateur Windows vers localhost, ou client Rust Windows vers le service WSL |

Le contrôle effectué ici trouve libcuda et NVRTC dans WSL, mais aucun ICD Vulkan NVIDIA identifié dans le répertoire système inspecté. Cela ne remplace pas un test d’adapter ; **l’accès matériel wgpu à la 3090 sous ce WSL n’est pas validé**. Le chemin CUDA évite d’en faire une hypothèse obligatoire.

Au démarrage, enregistrer nom de l’adapter, identifiant, backend, limites et type matériel. Un adapter logiciel ne satisfait pas le profil GPU de production. Un test court vérifie copie de buffer, calcul, réduction et récupération des données avant de démarrer un monde.

Pour le mode WSL, conserver runtime et index sous Linux, avec un seul processus écrivain. Le client Windows reçoit les données par le service ; il n’ouvre pas simultanément la base SQLite via un partage de fichiers. Pour Windows natif, conserver runtime et index sur le volume Windows.

La disponibilité du navigateur et de son WebGPU se vérifie dans l’environnement client, séparément de CUDA et du worker. WebGL2 peut maintenir un affichage réduit si le repli a été implémenté ; le client natif demeure l’autre chemin qualifié.

Sous WSL, le driver NVIDIA est fourni par Windows. La documentation NVIDIA demande de ne pas installer un driver GPU Linux concurrent dans WSL ; seul le toolkit de compilation adapté est nécessaire [R24][r24]. Aucun changement d’environnement n’est effectué par ce document.

### Séquence de création

1. Définir paramètres physiques et profil de société.
2. Produire les prévisualisations, avec un rapport de qualité et une graine récupérable.
3. Choisir une graine et accepter explicitement sa compilation finale comme une nouvelle révision.
4. Calculer l’atlas final et ses budgets, avec checkpoints.
5. Construire contrats, pôles et corridors ; préparer les zones qui conditionnent leur faisabilité.
6. Valider les critères de publication.
7. Épingler les régions d’intérêt à une résolution choisie.
8. Lancer le service et l’explorateur sur loopback.
9. Calculer les nouvelles régions en fond pendant l’exploration.

Les jobs doivent pouvoir reprendre au dernier checkpoint validé. Ils rapportent avancement, mémoire, quantité de disque prévue et erreurs. Une durée limite interrompt proprement le travail et conserve les checkpoints ; elle ne certifie pas un monde inachevé.

### Interface de commande prévue

Ces commandes sont des **exemples de contrat pour l’implémentation future** :

~~~bash
worldctl preview scenario.yaml --budget-seconds 180
worldctl compile scenario.yaml --profile final --resume
worldctl validate <WorldId> --suite publication
worldctl pin <WorldId> region.geojson --step-m 1
worldctl serve <WorldId> --port 8001 --cache-gb 30
~~~

La compilation sélectionne son backend avec une option explicite, par exemple --backend wgpu-dx12, wgpu-vulkan ou cuda. Le manifeste retient le résultat ; une détection automatique au démarrage ne remplace pas ce choix dans un monde déjà publié.

Une compilation interrompue peut reprendre avec les mêmes paramètres. Une reprise avec paramètres différents est refusée ou devient un nouveau job et une nouvelle révision.

### Reproductibilité numérique

Le plan topologique est produit avec des ordres stables et des décisions quantifiées sur CPU. Les générateurs aléatoires sont adressés par objet et position, pas consommés dans une séquence partagée. Les accumulations de graphes utilisent un ordre topologique déterminé.

Les noyaux évitent les atomiques flottantes dont l’ordre est variable. Les opérations transcendantales, compilateurs, flags et politique de contraction arithmétique font partie du profil numérique.

La cible initiale est la répétabilité binaire sur **la 3090, un backend GPU qualifié et un environnement verrouillé**. La portabilité sur d’autres matériels ou entre DirectX 12 et Vulkan exige des tests de tolérance et ne reçoit pas automatiquement une promesse binaire. Les bibliothèques ML documentent également des limites de reproductibilité entre versions et plateformes [R25][r25].

Une région épinglée reste exportable comme données canoniques. Si l’environnement numérique de référence n’est plus disponible, la régénération exacte de branches inédites doit être déclarée indisponible ou certifiée après qualification d’un nouveau profil.

### Exploitation

Le service refuse de commencer un calcul qui dépasse les quotas. Les paquets et l’index sont sauvegardés avec les manifestes, versions et modèles nécessaires. Les caches régénérables peuvent être supprimés par une commande dédiée ; les régions épinglées ont une protection séparée.

Les rapports doivent distinguer anomalie d’entrée, échec de contrat, non-convergence, manque de mémoire et corruption de paquet. Un redémarrage ne doit pas réessayer indéfiniment une géométrie impossible.

## 17 Apprentissage optionnel sur la 3090

L’apprentissage n’est pas une dépendance du monde initial. Il devient utile si les propositions procédurales manquent de variété ou coûtent trop cher, après réussite des contrats.

### Premier modèle proposé

Commencer par un petit réseau convolutionnel de type U-Net, d’environ **20 à 40 millions de paramètres**, qui propose un résidu d’altitude et des attributs de surface. Entrées : parent, géologie, pente, climat, budgets hydrologiques, distances aux crêtes et rivières, profils de lit, frontières et bruit spatial déterministe.

Sorties : résidu et distributions de matériaux. Elles sont conditionnelles au contrat. Le solveur de la section 7 reste obligatoire.

Une diffusion à grande échelle peut produire des exemples ou un style, mais un réseau de proposition à passage unique est un premier choix plus simple pour la latence. Le travail de Borg et al. fournit un point de comparaison planétaire et du code officiel [R12][r12], [R26][r26] ; il ne remplace pas les vérifications du présent système.

### Données et résolution réelle

Utiliser un petit corpus diversifié de reliefs naturels : montagne humide, terrain sec, plaine alluviale, côte et relief glaciaire. Les DEM au mètre de l’USGS 3DEP fournissent une source de terrain nu à cette échelle ; leurs métadonnées précisent aussi que les jonctions entre projets ne sont pas nécessairement sans raccord [R27][r27].

Une carte à 30 m peut aider sur les formes régionales, **pas apprendre seule des détails métriques observés**. Constituer les paires parent–enfant par réduction contrôlée de données réellement fines, avec géométrie, unités et masques documentés. Les exemples doivent passer le même nettoyage hydrologique que celui déclaré pour l’entraînement, sans supprimer les lacs réels.

Séparer entraînement, validation et test par bassins ou régions, et non par patches voisins tirés au hasard. Les lithologies inconnues sont marquées comme inconnues. Ne pas prétendre posséder une vérité géologique extraite automatiquement d’un DEM.

Une première collecte de quelques milliers de patches 256² à 512² peut tenir dans les 20 Go réservés si les canaux et duplications sont bornés. Vérifier le volume avant téléchargement.

### Coût et procédure

Commencer avec batch 1 à 4, précision mixte pour le réseau et gradient accumulation si nécessaire. Viser une allocation maximale de **12 à 16 Gio de VRAM**, sous réserve des autres processus. Le worker de génération peut être mis en pause pendant l’entraînement.

Mesurer le coût d’un epoch sur un sous-ensemble, puis décider du nombre d’epochs. Aucun temps d’entraînement fiable n’est déduit du seul nom de la carte graphique.

Une fonction de perte combine erreur d’altitude, gradients, spectre spatial, moments parent et violations de drainage. Les contraintes dures restent imposées à l’inférence ; leur pénalité dans la perte ne les garantit pas.

L’inférence s’effectue sur une unité canonique de génération, pas sur chaque tuile de rendu avec un bruit indépendant. Le hash des poids et le protocole d’inférence appartiennent au WorldId.

Pour garder la production en Rust, exporter le petit modèle en ONNX puis générer son module Rust avec burn-onnx, ou implémenter directement son architecture dans Burn. L’import ONNX de Burn produit du code Rust et des poids utilisables avec ses backends GPU [R31][r31]. Qualifier les opérateurs réellement exportés et comparer les sorties au modèle d’entraînement avant adoption.

Python peut donc entraîner ou comparer un modèle, mais l’inférence canonique n’appelle pas un serveur Python. Charger les poids une fois, retenir la version du moteur d’inférence et réserver sa mémoire dans le quota du worker. Le modèle reste côté natif ; le viewer WebAssembly n’en a pas besoin.

### Critère d’adoption

Adopter le modèle seulement s’il améliore la qualité sur des régions de test inédites, conserve le taux de certification et présente un gain mesuré après **inférence, projection et encodage**. Une belle image isolée ne valide pas le modèle.

Comparer trois variantes : procédural seul, réseau de proposition et éventuellement diffusion. Si le modèle multiplie les échecs de contraintes ou ne réduit pas le coût complet, conserver le générateur procédural.

## 18 Validation et expériences décisives

### Trois familles de vérité

Il faut distinguer :

1. **Conformité formelle du monde :** identités, interfaces, réduction, topologie et budgets.
2. **Plausibilité mesurée :** distributions géomorphologiques et réponses physiques aux paramètres.
3. **Qualité perçue :** un paysage lisible et convaincant, au globe comme au sol.

Un résultat peut satisfaire la première et échouer aux deux autres. Des rayures de quadtree parfaitement continues restent un mauvais terrain.

### Tolérances initiales proposées

Ces seuils sont des contrats à essayer, puis à figer dans une version. Ils ne sont pas des mesures obtenues sur le prototype actuel.

| Contrôle | Seuil de départ |
|---|---|
| Bordure décodée | Identité exacte des valeurs canoniques partagées |
| Moyenne d’altitude enfant vers parent direct | Erreur absolue maximale de 2 cm |
| Réduction cumulée depuis le niveau terminal | Borne propagée des erreurs, environ 24 cm pour douze liens à 2 cm |
| Déplacement des objets | Dans leur enveloppe, avec ancrages protégés identiques |
| Connexions importantes | Aucun changement de quotient de graphe |
| Drainage sec | Aucune remontée non autorisée après quantification ; plats déclarés |
| Bilan hydrologique numérique | Résidu relatif inférieur à 10⁻⁴, avec un plancher absolu déclaré |
| Interface de flux partagée | Même flux canonique, avec orientations opposées |
| Stocks de sédiments | Résidu relatif inférieur à 10⁻⁴ à la réduction, avec plancher absolu |
| Continuité visuelle | Pas de trou ; erreur de raccord sous un pixel à la distance évaluée |
| Recompilation sur profil de référence | Même hash de données, hors journaux et temps de calcul |

La borne cumulée n’est pas une précision topographique scientifique. Les premiers moments, les enveloppes et l’erreur de reconstruction sont rapportés séparément. Les débits presque nuls utilisent un seuil absolu afin d’éviter un faux diagnostic par division.

### Expérience locale prioritaire

Créer une région de référence contenant mer, côte plate, côte rocheuse, fleuve traversant un désert, confluent, lac avec seuil, bassin fermé et partage des eaux. Définir trois niveaux avec un rapport total de résolution d’au moins seize.

Répéter le calcul :

- parcours vers l’amont puis vers l’aval, et ordre inverse ;
- ordre aléatoire, compilation concurrente et interruption/reprise ;
- cache vide, cache chaud et éviction des voisins ;
- saison sèche et saison humide, sans changer le terrain ;
- toutes les orientations d’une arête sphérique et les huit coins du cube.

Vérifier les certificats, puis calculer le drainage à nouveau depuis la surface finale par un analyseur indépendant du planificateur. Ce second calcul doit retrouver les exutoires et bassins protégés. Comparer l’eau et les sédiments, pas seulement une capture d’écran.

Faire une coupe traversant frontière et fleuve pour vérifier relief, profil de lit, niveau d’eau et normales. Les coutures peuvent être hydrologiques ou lumineuses même si l’altitude est continue.

### Qualité géomorphologique

Comparer avec des DEM de test tenus à part :

- distributions de pentes, courbures et hypsométrie ;
- densité de drainage, aire contributive et rapports de branches ;
- relation aire–pente, formes de vallée et profils longitudinaux ;
- largeur des plaines alluviales et répartition des dépôts ;
- complexité des côtes selon la résolution ;
- spectre spatial et anisotropies alignées sur les tuiles.

La ressemblance statistique ne démontre pas une histoire géologique vraie. Elle aide à éliminer le bruit isotrope, les montagnes clonées et les réseaux trop réguliers.

Préparer des parcours fixes du globe au sol avec les mêmes éclairages et échelles. Évaluer les vues sans changer les réglages pour sauver une région. Conserver aussi les cas défavorables.

### Climat et physique

Tester rotation accélérée et ralentie, obliquité nulle puis forte, gravité et pression modifiées, eau réduite et système binaire. Vérifier conservation et sens des changements avant de chercher une reproduction précise d’un climat de référence.

Comparer plusieurs cas à ExoPlaSim et publier erreurs et domaine de calibration. Si le modèle réduit de vent ne produit pas une circulation raisonnable, utiliser des climatologies GCM précompilées dans le domaine supporté ou limiter le profil disponible. Ne pas certifier un climat exotique simplement parce que le solveur est stable.

### Performance et robustesse

Mesurer à froid comme à chaud le temps complet, la fermeture de dépendances, le pic de RAM, le pic de VRAM, les octets de cache et le temps de frame. Faire durer un parcours jusqu’à atteindre le plafond de cache.

Comparer le viewer web/WASM et le viewer natif avec les mêmes paquets, parcours, éclairage, résolution et backend de compilation. Mesurer temps de frame p50/p95, copies, décodage, upload et mémoire. Conserver le web comme cible principale si les deux atteignent le budget. Privilégier le natif si lui seul satisfait le budget, ou si son gain p95 est matériel — seuil proposé de 20 % — et utile au scénario d’exploration. Ne pas changer le monde pour favoriser l’un des clients.

Tester perte d’une connexion locale, corruption de paquet, arrêt pendant la publication, disque presque plein et client qui change rapidement de destination. Le monde validé reste lisible, le parent reste disponible et aucun paquet partiel ne devient canonique.

## 19 Plan d’implémentation et décisions de passage

Ne pas construire tous les modules avant d’avoir vérifié la méthode centrale. L’ordre ci-dessous permet d’arrêter tôt une mauvaise représentation.

| Étape | Livrable concret | Condition de passage |
|---|---|---|
| 1 Sphère et représentation | Projection, voisins, moments, stockage et viewer avec relief analytique | Toutes les faces et transitions sont continues ; précision vérifiée |
| 2 Preuve de raffinement | Région locale, interfaces uniques et solveur avec témoins | Réduction, reprise et ordre de génération conformes |
| 3 Drainage et côte | Bassins, fleuve, lac, bassin fermé et côte à trois niveaux | Analyseur indépendant et contrats retrouvent les structures |
| 4 Relief convaincant | Primitives, lithologie et propositions multi-échelle | Pas de bandes de frontière ; formes satisfaisantes sur cas de test |
| 5 Atlas planétaire | Prévisualisation et compilation finale | Budget global mesuré et publication reproductible |
| 6 Climat et eau saisonnière | Modèle réduit, budgets, sol et niveaux d’eau | Bilans contrôlés et domaine de plausibilité défini |
| 7 Exploration complète | Streaming, matériaux, rivières, mer et biomes | Navigation fluide sous charge et cache borné |
| 8 Géographie humaine | Quotas, implantations, corridors et ouvrages | Identités et faisabilité persistent au raffinement |
| 9 Extension apprise | Corpus, modèle local et comparaison | Gain démontré, contrats inchangés |

### Risques qui peuvent changer l’architecture

| Risque | Diagnostic | Réponse prévue |
|---|---|---|
| Contraintes trop rigides | Taux élevé de plans incompatibles ou détails pauvres | Améliorer les témoins et les enveloppes avant de figer l’atlas |
| Interfaces visibles | Anisotropie et bandes dans pentes ou drainage | Modifier colliers et domaines ; tester une génération par unités de bassin |
| Solveur trop lent | La projection domine le temps complet | Réduire contraintes redondantes, préconditionner et employer des bases de résidus admissibles |
| Cascade de dépendances | Une visite locale déclenche une part croissante du globe | Précompiler les plans structurels nécessaires et changer leur granularité |
| Climat peu plausible | Réponses erronées aux paramètres | Restreindre le domaine ou fournir des climatologies calibrées |
| Routes impossibles au détail | Réservation violée ou franchissement découvert trop tard | Préparer les passages critiques avant publication |
| Ressources insuffisantes | Cache ou GPU dépassent les plafonds | Réduire régions épinglées et concurrence, conserver le même monde |

Une alternative au solveur général est une base de résidus qui conserve déjà les moments et s’annule aux frontières, avec primitives de lit admissibles par construction. Elle peut diminuer la liberté et le coût. Ce choix doit être décidé par l’expérience de raffinement, pas après avoir réécrit tout le projet.

Conserver les prototypes actuels pour comparer des idées, données et images. Le nouveau noyau n’hérite pas implicitement de leurs raccords ou garanties. Réutiliser une formule ou un outil testé est possible ; réutiliser sa sémantique de génération sans preuve ne l’est pas.

Le présent livrable s’arrête à ce design. Les étapes, commandes, API, formats et seuils décrivent le travail futur.


## 20 Références scientifiques et techniques

Ces références soutiennent les briques nommées dans le texte. Elles ne démontrent pas l’architecture combinée. Pour certaines publications, la consultation a porté sur le résumé primaire et le code officiel lorsque le texte intégral n’était pas accessible ; les choix nouveaux restent explicitement proposés.

- **[R1 Génévaux et al. 2013][r1]**, *Terrain Generation Using Procedural Models Based on Hydrology*, ACM TOG 32(4), DOI 10.1145/2461912.2461996. Réseau hiérarchique, primitives et reconstruction de terrain.
- **[R2 Derzapf et al. 2011][r2]**, *River Networks for Instant Procedural Planets*, CGF 30(7), DOI 10.1111/j.1467-8659.2011.02052.x. Génération et raffinement adaptatif de réseaux sur une planète. Les temps publiés ne sont pas transposés à notre système.
- **[R3 Fischer et al. 2022][r3]**, *Procedural Generation of Landscapes with Water Bodies Using Artificial Drainage Basins*. Bassins artificiels, rivières et lacs comme éléments générateurs.
- **[R4 Cordonnier et al. 2016][r4]**, *Large Scale Terrain Generation from Tectonic Uplift and Fluvial Erosion*, CGF 35(2). Couplage soulèvement–incision ; page bibliographique des auteurs.
- **[R5 Schott et al. 2023][r5]**, *Large-Scale Terrain Authoring through Interactive Erosion Simulation*, ACM TOG 42(5). Travail dans le domaine du soulèvement et approximation de drainage.
- **[R6 Tzathas et al. 2024][r6]**, *Physically-based analytical erosion for fast terrain generation*, CGF 43(2), DOI 10.1111/cgf.15033. Érosion analytique, multigrille et optimisation ; la publication décrit notamment une limite de continuité temporelle à grand âge géologique. [PDF des auteurs](https://www-sop.inria.fr/reves/Basilic/2024/TGSC24/Analytical_Terrains_EG.pdf).
- **[R7 Schott et al. 2024][r7]**, *Terrain Amplification using Multi-scale Erosion*, ACM TOG 43(4). Incision, versants et processus thermiques à plusieurs échelles.
- **[R8 Barnes et al. 2014][r8]**, *Priority-Flood: An Optimal Depression-Filling and Watershed-Labeling Algorithm for Digital Elevation Models*, Computers & Geosciences 62. Analyse et traitement des dépressions.
- **[R9 Barnes et al. 2021][r9]**, *Computing water flow through complex landscapes – Part 3: Fill–Spill–Merge*, Earth Surface Dynamics 9, DOI 10.5194/esurf-9-105-2021. Stockage et routage dans les hiérarchies de dépressions.
- **[R10 Perche et al. 2025][r10]**, *Vector-Based Terrain Modelling*, CGF 44(6), DOI 10.1111/cgf.70160. Code officiel de la représentation vectorielle.
- **[R11 Huftier et al. 2026][r11]**, *Terrain Synthesis and Authoring based on Iso-Contours*, CGF 45(2), DOI 10.1111/cgf.70389. Courbes imbriquées et reconstruction ; page des auteurs.
- **[R12 Borg et al. 2026][r12]**, *Authoring Terrestrial Planets with Diffusion Models*, CGF 45(2), DOI 10.1111/cgf.70390. Synthèse planétaire conditionnée.
- **[R13 Paradise et al. 2022][r13]**, *ExoPlaSim: Extending the Planet Simulator for exoplanets*, MNRAS 511(3). GCM de complexité intermédiaire adapté à des configurations exoplanétaires.
- **[R14 Galin et al. 2010][r14]**, *Procedural Generation of Roads*, CGF 29(2), DOI 10.1111/j.1467-8659.2009.01612.x. Construction de chemins selon terrain et coût.
- **[R15 Asirvatham et Hoppe 2005][r15]**, *Terrain Rendering Using GPU-Based Geometry Clipmaps*, GPU Gems 2, chapitre 2. Grilles régulières, streaming et morphing de niveaux.
- **[R16 Documentation PROJ sur QSC][r16]**. Propriétés et distinction entre QSC et COBE QSC. Source de comparaison pour les projections, pas bibliothèque imposée au système.
- **[R17 Documentation FastScapeLib][r17]**. Incision, diffusion et transport avec solveurs implicites ; référence de comparaison numérique.
- **[R18 Code officiel MultiScaleErosion][r18]**. Implémentation Linux/Windows avec OpenGL compute, licence MIT affichée par le dépôt. Examiner les fichiers et dépendances avant un éventuel portage.
- **[R19 Documentation REBOUND][r19]**. Intégrateurs, notamment IAS15, pour les éphémérides multi-corps.
- **[R20 Barstad et Schüller 2011][r20]**, *An Extension of Smith’s Linear Theory of Orographic Precipitation: Introduction of Vertical Layers*, Journal of the Atmospheric Sciences 68(11), DOI 10.1175/JAS-D-10-05016.1. Délais et transport des condensats dans une extension du modèle linéaire.
- **[R21 Documentation ExoPlaSim][r21]**. Prérequis, compilation, compatibilité et fonctionnalités disponibles ou annoncées. Vérifier la version réellement utilisée.
- **[R22 Documentation wgpu][r22]**. API Rust pour rendu et calcul, backends natifs et navigateur. [Documentation du crate](https://docs.rs/wgpu/latest/wgpu/).
- **[R23 Bruneton 2017][r23]**, *Precomputed Atmospheric Scattering: a New Implementation*. Paramètres, densité, spectre, unités et exemple WebGL2.
- **[R24 Documentation NVIDIA CUDA sous WSL][r24]**. Séparation entre driver Windows et toolkit de compilation Linux.
- **[R25 Documentation PyTorch sur la reproductibilité][r25]**. Limites entre matériels et versions, sources de hasard et algorithmes déterministes.
- **[R26 Code officiel PlanetDiffusion][r26]**. Installation et accès à des modèles publiés associés au travail de Borg et al. Les conditions des poids et des données doivent être vérifiées séparément.
- **[R27 Catalogue USGS des DEM 3DEP au mètre][r27]**. Données de terrain nu, couverture, références spatiales et limites de raccord entre projets.
- **[R28 Documentation Axum][r28]**. Framework HTTP Rust intégré à Tokio.
- **[R29 Documentation cudarc][r29]**. API Rust de CUDA, dont NVRTC et lancement de kernels. [Module NVRTC](https://docs.rs/cudarc/latest/cudarc/nvrtc/index.html).
- **[R30 Documentation winit et egui][r30]**. Fenêtrage et événements Rust ; [egui](https://github.com/emilk/egui) pour une interface Rust disponible en natif et sur le web.
- **[R31 Documentation Burn ONNX][r31]**. Génération de code Rust et de poids depuis ONNX, avec backends CPU et GPU ; vérifier la couverture des opérateurs du modèle retenu.

[r1]: https://perso.liris.cnrs.fr/eric.galin/Articles/2013-river-networks.pdf
[r2]: https://www.mathematik.uni-marburg.de/~derzapf/public/2011/TG.pdf
[r3]: https://cgvr.cs.uni-bremen.de/papers/cgi22/CGI22.pdf
[r4]: https://perso.liris.cnrs.fr/apeytavi/website/publication/hal-01262376/
[r5]: https://aparis69.github.io/public_html/projects/schott2023_Uplift.html
[r6]: https://www-sop.inria.fr/reves/Basilic/2024/TGSC24/
[r7]: https://aparis69.github.io/public_html/projects/schott2024_Erosion.html
[r8]: https://rbarnes.org/sci/2014_depressions.pdf
[r9]: https://esurf.copernicus.org/articles/9/105/2021/
[r10]: https://github.com/Arches-Team/vector-based_terrain_modelling
[r11]: https://perso.liris.cnrs.fr/apeytavi/website/publication/hal-05608822/
[r12]: https://onlinelibrary.wiley.com/doi/10.1111/cgf.70390
[r13]: https://academic.oup.com/mnras/article/511/3/3272/6516962
[r14]: https://onlinelibrary.wiley.com/doi/10.1111/j.1467-8659.2009.01612.x
[r15]: https://developer.nvidia.com/gpugems/gpugems2/part-i-geometric-complexity/chapter-2-terrain-rendering-using-gpu-based-geometry
[r16]: https://proj.org/en/stable/operations/projections/qsc.html
[r17]: https://fastscape.org/fastscapelib-fortran/
[r18]: https://github.com/H-Schott/MultiScaleErosion
[r19]: https://rebound.hanno-rein.de/integrators/
[r20]: https://journals.ametsoc.org/abstract/journals/atsc/68/11/jas-d-10-05016.1.xml
[r21]: https://exoplasim.readthedocs.io/en/latest/
[r22]: https://wgpu.rs/
[r23]: https://ebruneton.github.io/precomputed_atmospheric_scattering/
[r24]: https://docs.nvidia.com/cuda/wsl-user-guide/
[r25]: https://docs.pytorch.org/docs/2.14/notes/randomness.html
[r26]: https://github.com/Oliver-Borg/PlanetDiffusion
[r27]: https://data.usgs.gov/datacatalog/data/USGS%3A77ae0551-c61e-4979-aedd-d797abdcde0e
[r28]: https://docs.rs/axum/latest/axum/
[r29]: https://github.com/chelsea0x3b/cudarc
[r30]: https://docs.rs/winit/latest/winit/
[r31]: https://burn.dev/blog/release-burn-onnx-0.21.0/

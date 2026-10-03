# Revue du détail côtier sphérique

Revue du 3 octobre 2026, actualisée pour le prototype 0.8.0. **Claude Opus 5.5** a réellement
été appelé en CLI, en lecture seule ; le modèle effectif `claude-opus-5-5` a été
vérifié dans sa sortie. La revue couvre `coast.rs`, le sampler, la configuration
et leurs interactions avec les réservoirs et les rivières. Une
[seconde revue CLI](REVIEWS/OPUS_COAST_0_8_2026-10-03.md) a examiné une version
intermédiaire du nouveau spectre. Les contrôles GPU sont séparés dans les
[mesures 0.8](PERFORMANCE_0_8.md).

## Construction et propriétés

Pour un axe unitaire fixe `a`, un twist applique
`T(p) = rotation(a, θ(a·p)) × p`. La rotation conserve `z = a·p` : son inverse
est donc la rotation d’angle opposé, évaluée avec le même `z`. Une bande combine
trois axes semés indépendamment ; les deux premiers sont suffisamment séparés.
L’inversion parcourt bandes et rotations dans l’ordre inverse. Les axes et les
graines ne dépendent pas de la caméra.

En coordonnées axiales et azimutales, `(z, φ) → (z, φ + θ(z))`. L’élément d’aire
sphérique est `dz dφ`, et le déterminant jacobien vaut 1. En arithmétique exacte,
chaque twist et leur composition sont donc inversibles et préservent l’aire,
y compris quand la pente du bruit augmente. Le plafond de force limite la
déformation ; il n’est pas nécessaire à l’inversibilité.

Le relief naturel reconstruit `h₀(T(p))`. Le détail vertical est borné par
`0,8 × |h₀(T(p))|`, avec un bruit normalisé borné par 1 : il ne change pas le
signe marin. **La connectivité du contour analytique zéro est conservée.**
Cette propriété porte sur le relief remappé et son détail vertical, avant les
contraintes de berges lacustres et l’incision fluviale ; elle
ne prouve pas la topologie des triangles qui sous-échantillonnent le contour.

## Spectre et paramètres

Les bandes utilisent une fréquence nominale `fᵢ = 0,75 × N × 2ⁱ`. Chaque
twist la multiplie par un facteur semé indépendant entre 0,85 et 1,20 et possède
son propre déphasage. Son coefficient angulaire vaut
`min(0,55 × ratio × 2^(0,32i), 0,45 × sqrt(ratio)) / fréquence`.
L’amplitude suit approximativement `λ^0,68` avant le plafond, puis `λ` ; le
plafond reste sensible au réglage de détail. Il borne un coefficient, pas
le jacobien de toute la composition.

Le bruit utilise des points espacés irrégulièrement de 0,5 à 1,5 unités. Une
interpolation Hermite et des dérivées partagées assurent C1, sans imposer une
pente nulle à chaque point d’une grille régulière. Ces changements réduisent
le motif de bandes croisées de l’ancienne composition ; ils ne prouvent pas
une isotropie visuelle parfaite de tous les voisinages.

Le cutoff `coast_reference_spacing_m`, **0,25 m** par défaut, est fixe pour le
monde. Les twists dont la longueur d’onde réelle passe sous ce cutoff sont
omis. Les coefficients immuables sont conservés par thread, avec une clé
incluant tous leurs paramètres ; les échantillons gardent le même ordre
d’évaluation, indépendamment des caches et de la caméra.

`coast_detail_ratio` règle la force des twists indépendamment de `detail_m`,
qui contrôle le détail vertical. Si le cutoff dépasse
la bande initiale, aucune bande n’est appliquée. Changer ces paramètres définit
un autre champ ; zoomer évalue toujours le même champ.

## Vérifications 0.8

Les cinq régressions `cargo test -p world-core coast::tests` passent : inversion
et norme sur directions aléatoires, faces et pôles ; déplacement métrique des
bandes fines ; distribution des axes sur 2 048 graines ; identité entre
threads et après changement de configuration ; continuité des valeurs et
dérivées aux points irréguliers. La sonde de l’atlas 0.8 retrouve **1 200 points
communs identiques** sur deux parcours L0→L24. Dans la fenêtre côtière 10 km,
les sept grilles de 8 à 512 intervalles gardent tous leurs points partagés.
L’écart de classification entre les triangles grossiers et la grille 512
passe de 1,24 % à 0,020 % : le champ est stable, sa discrétisation converge.

Le contrôle à 5 m a révélé un seuil de shader masquant les profondeurs
inférieures à 1 cm. Ce seuil déplaçait le rivage des plages très plates ;
l’eau est maintenant découpée à profondeur zéro.

## Vérifications historiques 0.7

- Les deux tests `cargo test -p world-core coast::tests` passent : inversion
  sur directions aléatoires, faces et pôles ; déplacement métrique des bandes
  fines pour la graine 2386084681.
- Un probe Rust séparé, sans modification du code source, contrôle **243 000
  directions** : graines 0/user/`u64::MAX`, rayons 10 km/1 737 km/100 000 km,
  résolutions 8/512/1024, cutoffs 0,25/1/10 000 m et ratios 0/0,16/1.
  Erreur inverse maximale : **6,88 × 10⁻¹⁵** sur la sphère unité ; erreur
  physique maximale sur ces configurations : **0,58 µm**.
- Microbenchmark natif de la transformation seule, 100 000 directions :
  ancien warp **67/54 ms**, twists **107/92 ms**, respectivement en 128/512.
  Ce contrôle ne mesure ni reconstruction complète ni temps GPU. Les twists
  utilisent une seule reconstruction, mais sont désormais évalués partout.

## Points d’intégration et limites

La revue a détecté une sélection de plan d’eau utilisant `p` alors que son
stencil venait de `T(p)`. Le sampler mémorise maintenant `atlas_direction` et
classe les candidats dans ce même espace. La révision de surface a également
été changée pour séparer les anciennes tuiles des nouvelles.

Les nœuds et voisins utilisés pour les tangentes fluviales sont maintenant
transportés par l’inverse commun, avec un cache temporaire sparse. Les
confluences gardent des nœuds communs, et le stencil remappé trouve leurs
chenaux. Une Hermite entre nœuds transportés approxime le transport de la
courbe entière ; son profil est ensuite adapté au terrain naturel. Les
régressions activent les twists et vérifient que les nœuds reviennent aux
coordonnées atlas après remapping.

La protection de signe concerne le niveau marin zéro, pas tous les niveaux
lacustres. Les petits détails d’un lac et les maillages grossiers nécessitent
leurs propres contrôles. Le twist conserve les composantes existantes ; il
ne crée pas un archipel absent du contour global. Les axes et fréquences
varient maintenant par bande ; chaque twist reste néanmoins un champ axial
unidimensionnel. Cette déformation ne supprime pas à elle seule les directions
préférentielles du drainage sur l’atlas physique grossier. Villes et routes
restent hors du périmètre de cette passe.

# Surface multiechelle 0.10

## Champs fixes, échantillonnage variable

Relief, côtes, populations insulaires, bassins et microclimat dépendent de la
graine et de coordonnées sphériques. Le zoom change l'échantillonnage et le
filtrage des couleurs ; il ne sélectionne pas une nouvelle géographie. Les
régressions comparent les points communs entre résolutions, les visites en ordre
inverse et les résultats après éviction des caches.

Le champ vertical utilise une interpolation quintique, des repères tournés
entre octaves et des crêtes arrondies. L'interpolation a des dérivées premières
et secondes continues aux frontières du bruit : les raccords de grille ne
créent plus de cassures de normales à cet endroit. Ce choix suit la correction
décrite par [Ken Perlin dans GPU Gems](https://developer.nvidia.com/gpugems/gpugems/part-i-natural-effects/chapter-5-implementing-improved-perlin-noise).
Il ne remplace pas l'érosion physique globale et ne constitue pas une
simulation de glacier.

Le signal reste borné par 1. Les plafonds d'amplitude du sampler conservent
le signe terre/mer et le niveau des réservoirs existants. La glace n'amplifie
plus artificiellement les crêtes du substrat. Les biomes terrestres sont
reconstruits depuis température et humidité continues, avec des perturbations
fixes de 35 km et 4 km ; les fractions d'océan et de lac restent séparées.

## Géographie et matériaux

- Les îles occupent sept bandes fixes, des îlots aux grandes îles. Les objets
  offshore reposent sur des fondations plus larges ; les gardes examinent tout
  leur support près des réservoirs physiques.
- Les fjords suivent des axes courbes ramifiés avec des raccords tangents,
  plutôt que des segments rectilignes réguliers.
- Quatre populations lacustres couvrent des diamètres nominaux de 10 m à
  3 km. Chaque bassin conserve un identifiant, un fond et un plan d'eau fixes.
- Les couleurs mélangent couvert végétal, clairières, sol, roche et neige sur
  six bandes de 64 km à 2 m. Les bandes sous le seuil de résolution sont
  progressivement filtrées. La pente physique conditionne les couverts.

Voir [les biomes](BIOMES_0_10.md), [les côtes et îles](COAST_0_10.md) et
[les mesures lacustres](QUALITY_REFERENCE.md) pour les paramètres et limites.

## Triangle côtier soulevé

Le problème est géométrique : un bord fin pouvait être collé à la corde d'un
voisin plusieurs niveaux plus grossier. Un fond marin ainsi relevé devenait
visible quand la profondeur interpolée de la mer devenait négative. Sa couleur
terrestre expliquait le triangle vert, sans déplacement du champ côtier.

La régression reproduit un soulèvement de plus de 30 m sur une côte de pente
0,119 m/m avec un voisin L5 pour un bord L15. Le même point raccordé à L14
présente un écart inférieur au millimètre. Les subdivisions demandées sont
fermées sous leur support 2:1 ; les coupes proposées lors du raffinement et du
regroupement conservent aussi cette contrainte. La [revue Opus 5.5](OPUS_REVIEW_0_10.md)
analyse le défaut et la [revue Sonnet 5.5](SONNET_REVIEW_0_10.md) examine la
correction. Une cellule grossière peut encore sous-échantillonner une petite baie.

## Caches et compatibilité

Les petits objets sont décrits à des adresses canoniques ; seuls ceux dont le
support peut influencer la requête sont validés. Les résultats rejetés et les
validations partagées sont aussi mis en cache. Les quotas restent bornés et
sont séparés par taille ; huit partitions spatiales réduisent l'attente entre
threads. Les marges intérieures des protections physiques permettent de sauter
les populations forcément rejetées. La protection d'un canal est bornée aussi
par la distance à la sortie de son stencil. Une éviction change le coût du
calcul, jamais le résultat.

`prototype-0.10.0` identifie le générateur ; `spherical-coast-10` invalide les
tuiles de surface précédentes. Le paquet GPU préparé `WVP2` ajoute fractions
de biomes et coordonnées précises ; le worker conserve les données natives
avant préparation, avec la révision de surface dans sa clé.

Les petits lacs et les fondations insulaires constituent une reconstruction
visuelle. Leurs volumes et exutoires ne sont pas intégrés au bilan hydrologique
physique de l'atlas. Les vallées de fjord sont procédurales ; elles ne proviennent
pas d'un solveur d'écoulement de glace.

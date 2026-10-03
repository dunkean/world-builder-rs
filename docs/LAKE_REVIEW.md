# Revue des domaines lacustres

Une revue réelle en CLI avec Opus 5.5 a examiné `lake_geometry.rs`,
`sampler.rs`, `lib.rs` et leurs interfaces hydrologiques. La revue était
en lecture seule et n'a pas exécuté les tests. Les constats ci-dessous
combinent cette lecture avec nos régressions et sondes numériques ; ils
ne constituent pas une preuve universelle de naturalité ou de topologie.

## Corrections issues de la revue

| Constat | État et correction |
| --- | --- |
| Eau, bordure et crête choisissaient des propriétaires avec des métriques différentes | Corrigé : rayon normalisé unique, y compris avec des aires de cellules différentes. |
| Une paire de lacs non propriétaires pouvait créer une crête sèche dans un troisième lac | Corrigé : influence conditionnée par les deux domaines les plus proches. |
| Les bassins sous le niveau marin retombaient sur le support carré du quad et parcouraient tous les réservoirs | Corrigé : même domaine métrique, labels immuables et requête locale bornée. |
| La garde de berge écrasait les creux du détail sur une terrasse plate | Corrigé pour la texture : soulèvement du relief de base conservant la variation du détail. Le terrain de retenue reste une approximation artificielle. |
| Le helper des tests gardait des lacs issus de la génération initiale | Corrigé : réservoirs supprimés et caches invalidés avant la construction des scènes synthétiques. |
| Une incision fluviale peut traverser une interface hydraulique | Corrigé pour les cas identifiés : contrôles fixes, backwater adaptatif, dernière rive de sortie et détail borné aux contrôles secs. Les sondes ne prouvent pas toutes les interfaces. |

Les clones partagent le cache de labels par `Arc`. Une mutation publique
des cellules ou des réservoirs nécessite `invalidate_geometry()`. La
relecture d'un niveau ne remplace pas cette invalidation après changement
de membership, d'identité ou d'ordre des réservoirs.

## Vérification

Les régressions contrôlent des lacs d'aires inégales, trois réservoirs
voisins, une frange endoréique sous le niveau marin, le maintien du détail
sur la berge et les points aux arêtes et sommets du cube. La recherche
locale des supports y est comparée à une recherche exhaustive.
Les régressions supplémentaires empêchent le détail d'effacer un contrôle
de déversement sec et séparent un lac sous le niveau marin du fallback
océanique. Ces régressions passent dans la suite `world-core`.
La validation finale compte **71 tests core verts**, dont section isolée
plane jusqu'à 99,5 % de la largeur, deux sections au même niveau et
confluence de tributaires à 160/110 m vers un nœud à 100 m.

```sh
cargo test -p world-core
cargo run --release -p world-core --example quality_probe -- data/worlds/32ede09abc580c1b3049/atlas.json
cargo run --release -p world-core --example quality_probe -- data/worlds/32ede09abc580c1b3049/atlas.json --overlap-reference=examples/overlap-regression-2386084681.json
```

## Limites mesurées

Le contrôle porte sur la graine 2386084681 à 512² par face. L'atlas
contient 284 réservoirs et 3 334 cellules lacustres mouillées. Les
changements de propriétaire sont cherchés sur 500 100 positions autour
de ces cellules, puis localisés à moins de 1 mm. Les points communs
parent/enfant sont identiques sur 1 200 échantillons complets de L0 à L24.

La sonde finale des surfaces d'eau mesure 14 156 changements de propriétaire,
dont 23 paires mouillées des deux côtés. Les grands sauts précédents,
jusqu'à 101,8 m, et les raccords latéraux lac/rivière identifiés ne sont
plus observés : **zéro saut supérieur à 10 cm**, aucune différence d'eau
mesurable à la précision `f32` sur ces 23 paires mouillées.
La sonde des 512 grands tronçons,
sur 15 872 positions, trouve une profondeur fluviale maximale de
2,6701 m et aucun plan d'eau à plus de 10 m au-dessus du terrain naturel.
Ces résultats n'épuisent pas toutes les interfaces possibles.
Un contrôle complémentaire a révélé de vraies marches internes aux
confluences : sol jusqu'à 19,58 m et eau jusqu'à 1,10 m, à moins de 1 mm.
Les profils sont maintenant plafonnés sous les surfaces des autres
vallées, puis le sampler reconstruit continûment lit et profondeur.
Il garde le plan d'une section isolée et celui de deux sections au même
niveau ; les datums lac/mer restent exacts. Les métadonnées de propriétaire
ne choisissent aucune hauteur.

La sonde finale reprend **les mêmes dix voisinages d'origine** :
28 980 points, 620 frontières owner/mouillage, maximum du sol **0,061 mm**,
aucune différence d'eau mesurable à la précision `f32` sur 106 paires
mouillées. La profondeur locale maximale vaut 2,1408 m. Le mélange borne
la profondeur par le maximum des contributeurs, pas par la profondeur
du propriétaire affiché ; l'écart à ce dernier atteint 18,0 mm.
Les 57 écarts entre profil individuel et champ reconstruit atteignent
3,3062 m avec le même propriétaire. Continuité et profondeur bornée
ne prouvent pas la monotonie hydraulique du champ mélangé.

La reconstruction seule, sans détail vertical ni incision, relève
604 des 133 360 points examinés près des lacs, soit 0,453 %. Parmi ces
points, le soulèvement médian vaut 2,14 m, le P95 51,21 m et le maximum
145,18 m. Aucun des huit sommets du cube n'est modifié dans ce monde.
Cette mesure montre que la garde n'est pas toujours négligeable : elle
peut encore produire des rives artificielles et ne prouve pas un paysage
lacustre naturel. Son remplacement par une reconstruction contrainte
du vrai drainage à une résolution régionale reste nécessaire pour
atteindre la référence locale de 10 km.

Les caps seuls mesurent 5,43 s de compilation froide du catalogue ; la
sonde finale mesure 9,87 s pendant les builds natif et WASM concurrents.
L'analyse finale prend 8,89 s sous cette charge, chargement JSON exclu.
Ces durées CPU incluent les sondes de
soulèvement et ne décrivent pas la cadence du visualiseur.

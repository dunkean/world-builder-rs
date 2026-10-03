# Géographie multiéchelle — prototype 0.9

Cette révision ajoute des formes fixes au champ `coherent` et un rendu
cartographique. Le monde, les tuiles et les points d’exploration évaluent la
même géographie. Ni la caméra ni la résolution demandée ne créent un objet.

## Paramètres et exploration

| Paramètre JSON | Défaut | Effet |
| --- | --- | --- |
| `fjord_strength` | 0,65 | Profondeur et largeur des fjords aux hautes latitudes |
| `archipelago_density` | 0,60 | Acceptation des îles dans les chaînes côtières |
| `regional_lake_density` | 0,55 | Acceptation des petits bassins des régions glaciaires |

Les trois paramètres acceptent `0..1` ; zéro désactive la famille concernée.
Ils figurent dans le panneau Génération, ses préréglages et l’import/export.
Régénérer pour les appliquer. Les destinations de l’onglet Vue sont vérifiées
par `World::sample` : fjord marin, groupe d’îles émergées, vrai micro-lac et
intérieurs de chenaux fluviaux. Les rivières s’activent lors de leur visite.

## Champ géographique

`landforms.rs` sélectionne des districts dans l’atlas gelé. Les fjords sont
des vallées en U ramifiées, reliées à une rive trouvée par bissection. Ils
creusent le terrain après les îles pour préserver leur ouverture. Les archipels
suivent des arcs courbes avec grandes îles et petits îlots ; leurs hauteurs
sont bornées par leur largeur. Les régions lacustres favorisent des terres
humides, froides et peu pentues, avec un détail vertical moins intense.

`local_water.rs` adresse des bassins de 50 m à 3 km par des propositions
sphériques fortement décalées. Des lobes courbes et des bras latéraux composent
leurs contours. Chaque bassin reçoit un identifiant et un plan horizontal
fixes, après contrôle du terrain à son centre, sur ses lobes et ses franges.
Des partages secs continus séparent les plans différents ; les franges humides
contribuent au biome marais. Les indices et caches bornés ne changent que le
coût de calcul. Les rejets sont eux aussi réutilisables.

Tous ces objets vivent dans le domaine de l’atlas et suivent le même transport
côtier inversible. Un index sphérique protège les réservoirs physiques et leurs
seuils ; les nouveaux reliefs s’atténuent continûment à leur approche. Les
supports entiers des petits bassins sont exclus de ces domaines. Les saisons
utilisent une géographie décidée avec le climat annuel.

## Rendu, validation et limites

Voir [le rendu cartographique](RENDERING_0_9.md),
[les mesures de lacs](QUALITY_REFERENCE.md),
[les contrôles fluviaux](RIVER_REVIEW.md) et
[la revue Opus et ses corrections](REVIEWS/OPUS_GEOGRAPHY_0_9_2026-10-03.md).
Les tests couvrent les points partagés, l’ordre des visites, les frontières des
indices, les contacts entre plans, les seuils et les clones saisonniers.

La validation de cette révision compte 176 tests Rust et 20 tests JavaScript.
Sur l’atlas `512² × 6`, graine `2386084681`, une fenêtre lacustre de
`10 × 10 km` contient 42 bassins distincts et 5,692 % d’eau. Ses points
communs restent identiques entre trois résolutions et deux ordres de visite.
Le catalogue hydraulique froid complet coûte 3,650 s après chargement JSON ;
263 169 échantillons fins de cette fenêtre coûtent ensuite 1,310 s. Ce sont
des durées CPU, séparées des mesures navigateur du document de rendu.

Captures du navigateur : [fjords](screenshots/0_9/fjords.png),
[archipel](screenshots/0_9/archipelago.png),
[petits lacs](screenshots/0_9/small-lakes.png) et
[rivière](screenshots/0_9/river.png).

Les nouveaux objets sont une reconstruction procédurale de surface. Les
statistiques et bilans de conservation décrivent l’atlas physique : ils ne
comptent pas les nouveaux micro-lacs, fjords ou îles. Les petits lacs n’ont pas
encore d’apports et d’exutoires simulés ; les îles ajoutées n’ont pas de réseau
fluvial calculé. Le drainage global conserve sa résolution d’atlas. Le premier
calcul d’une région reste asynchrone ; son coût doit être distingué de la
cadence du rendu et de la réutilisation en cache.

Les formes s’inspirent des vallées glaciaires noyées décrites par
[l’USGS](https://pubs.usgs.gov/of/2004/1216/text.html) et des paysages lacustres
et humides du Bouclier canadien décrits dans
[Wetlands of Canada](https://publications.gc.ca/collections/collection_2019/eccc/En73-3-24-eng.pdf).
Le prototype ne simule pas l’évolution d’une calotte glaciaire.

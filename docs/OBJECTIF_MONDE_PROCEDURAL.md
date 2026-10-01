# Monde procédural planétaire cohérent du globe au mètre

Date : 1er octobre 2026. Statut : cadrage, fusion des prototypes et étude bibliographique ; aucune nouvelle architecture implémentée.

## 1. Objectif à atteindre

Créer un site d’exploration comparable, dans ses interactions, à Google Earth/Maps : une planète générée, parcourue par rotation, déplacement et zoom continu, jusqu’à une résolution au sol d’environ **un mètre par pixel**. Son rayon est paramétrable, avec une référence proche de la Lune, environ 1 737 km. Cette taille ne signifie pas reproduire l’environnement lunaire : gravité, atmosphère, eau et composition sont des paramètres distincts.

Le résultat attendu est **un monde unique révélé progressivement**. Relief, côtes et réseaux persistent lorsque l’on change d’échelle. La caméra choisit les données à calculer et à afficher ; elle ne choisit pas quel monde existe.

### Domaine et priorités

1. **Relief procédural zoomable** : continents, océans, chaînes, bassins, vallées, versants et détails métriques, avec formes géomorphologiques cohérentes.
2. **Hydrologie** : drainage, fleuves, affluents, lacs, zones humides, ruissellement et éventuels bassins endoréiques, compatibles avec relief et climat.
3. **Implantations et routes** : villes et villages déterminés par eau, sols, ressources et accessibilité ; transports compatibles avec pentes, cols, franchissements et échanges.

Géologie et climat sont des contraintes du modèle dès la conception, même si leurs interfaces détaillées arrivent plus tard. Lithologie, érodabilité, perméabilité et dépôts ne sont pas de simples couleurs : ils modulent relief, drainage, sols et ressources. Les implantations demandent aussi des hypothèses explicites de technologie, population et économie ; les ressources seules ne les déterminent pas.

Le climat doit accepter rayon, gravité, atmosphère, rotation, obliquité, éléments orbitaux, flux et spectre stellaires, ainsi que la phase saisonnière. Plusieurs soleils nécessitent un modèle orbital cohérent et l’addition de leurs contributions radiatives. L’obliquité seule ne détermine pas des saisons réalistes. On vise une plausibilité physique mesurable avec un niveau d’approximation annoncé.

Nomination, récits, factions et génération textuelle sont hors objectif. Grottes et volumes souterrains ne sont pas nécessaires au premier résultat : une surface d’altitude et des attributs de sol suffisent pour commencer.

### Reformulation algorithmique

Construire une **représentation hiérarchique déterministe sous contraintes**, combinant champs continus, rasters locaux et graphes/vectoriels persistants. Graine, version d’algorithme, paramètres physiques et position identifient le même monde, indépendamment du parcours, du cache et de l’ordre des requêtes.

Pour un niveau `l`, écrire conceptuellement `H_(l+1) = U(H_l) + r_l`, où `U` reconstruit le parent et `r_l` apporte les détails. L’opérateur de réduction `D` doit satisfaire `D(H_(l+1)) ≈ H_l` selon une tolérance définie. Une faible erreur d’altitude ne garantit cependant ni une côte stable ni un même bassin versant : il faut aussi conserver exutoires, connexions des fleuves, lignes de partage des eaux, corridors côtiers et identités des objets.

« Ne pas altérer le niveau inférieur » signifie conserver **sa représentation agrégée et ses objets structurants**. Sinuosités et anses nouvelles peuvent déplacer une géométrie dans une enveloppe compatible avec l’erreur du parent. Exiger des côtes absolument immobiles et de nouveaux détails serait contradictoire. Enveloppes, opérateurs de réduction et seuils de visibilité doivent être spécifiés par couche.

Érosion et hydrologie ne se calculent pas indépendamment sur chaque tuile : elles dépendent de l’amont, des exutoires et des voisins. Le raffinement reçoit des conditions aux limites — altitudes, flux d’eau et de sédiments, bassins, lithologie — puis respecte ces contraintes. Les saisons modifient les débits ; le changement de zoom ne refait pas l’histoire géologique.

Sur une sphère de rayon 1 737 km, `4πR² ≈ 3,79 × 10¹³ m²`. Un raster global d’altitudes à 1 m en float32 représenterait environ **152 To décimaux**, avant les autres couches. Ce calcul impose génération locale à la demande et caches bornés. Une résolution métrique n’est pas une précision scientifique métrique.

« Temps réel » porte d’abord sur navigation et affichage : génération asynchrone des régions utiles, maintien du parent pendant le calcul et réemploi des résultats. Il ne requiert pas une simulation planétaire métrique à chaque image. Budgets de première génération, latence de tuile, mémoire et fréquence d’affichage doivent être mesurés sur une machine cible ; 60 Hz peut être une cible de navigation, pas une promesse actuelle.

### Critères de réussite

| Propriété | Vérification attendue |
|---|---|
| Permanence | Même région après parcours différents et vidage du cache ; tolérance numérique documentée entre matériels. |
| Parent/enfants | Réduction des enfants vers le parent, avec erreurs bornées d’altitude, côte et attributs. |
| Continuité | Altitudes et flux compatibles entre tuiles, niveaux et faces de la sphère. |
| Hydrologie | Exutoires et connexions persistants, bilan d’eau explicite ; aucune remontée artificielle introduite par raffinement. |
| Unités physiques | Distances, pentes, aires, débits et érosion en unités physiques ; métrique locale de projection vérifiée. |
| Exploration | Précision des coordonnées jusqu’au mètre, transitions sans trou, file de calcul bornée et annulation des requêtes obsolètes. |
| Extensions | Identifiants stables des villes/routes ; apparition selon leur importance et raffinement des objets existants. |

## 2. Ce que la fusion conserve et ce qu’elle ne démontre pas

Le dossier canonique est `world_builder/`. `orogen/` est l’unique copie active du prototype global ; il conserve les ajouts de LOD présents dans World Builder. L’ancien chemin `projects/planet_heightmap_generation` pointe vers cette copie. Les sources Windows restent intactes.

- **Orogen** apporte terrain tectonique, érosion globale, climat saisonnier, import/export et exploration. Son fork ajoute des points proches de la caméra et reconstruit un maillage adaptatif. Ce raffinement par interpolation/bruit ne prouve pas la stabilité hydrologique au mètre (`orogen/js/adaptive-mesh.js`).
- **World Builder** apporte graphe global, tuiles parent/enfants, érosion GPU/CPU, analyse hydrologique, API et visualiseur LOD. Son raccordement peut modifier les voisins déjà en cache (`crates/world-builder-erosion/src/tile_generator.rs`, `stitch_edges`) : indépendance à l’ordre de génération et conservation parent/enfants restent à établir.
- Les moteurs Rust et JavaScript et les visualiseurs alternatifs sont des expériences distinctes conservées. En supprimer un comme « doublon » ferait perdre une approche ; les réunir dans un dépôt n’unifie pas leurs modèles physiques.

Les fichiers identiques d’Orogen ne sont conservés qu’une fois. Les différences ont été arbitrées en gardant le fork enrichi, puis en intégrant le lanceur, l’image manquante et les corrections Linux de la première migration. Inventaire et empreintes : `docs/provenance/merge-manifest.json`. Historique amont d’Orogen : `orogen-upstream.bundle`. World Builder n’avait pas encore de commit : son index et ses modifications locales sont conservés. Caches et binaires Windows sont exclus de la copie Linux.

Les anciens documents restent comme historique ; leurs déclarations de phases terminées et de performance ne valident pas ce nouvel objectif. La GPL d’Orogen est conservée dans `orogen/LICENSE` ; les composants restent identifiables par origine.

## 3. Rapport bibliographique — trois pages de synthèse

Les trois parties suivantes constituent la synthèse, références comprises. Les limites de page sont indicatives en Markdown ; les ruptures servent à l’export. Recherche ciblée de sources primaires consultées le 1er octobre 2026. Aucune solution complète à toutes les exigences n’est établie par ces travaux. Les recommandations sont une synthèse de conception, pas des garanties publiées.

### Page 1/3 — Structure géomorphologique et amplification

**Le drainage comme structure génératrice.** Génévaux et al., *Terrain Generation Using Procedural Models Based on Hydrology*, ACM TOG 32(4), 2013, construisent un réseau hydrologique hiérarchique vectoriel, puis combinent primitives de relief et de rivière. Base directement pertinente : le fleuve devient une contrainte persistante plutôt qu’un tracé réinventé sur chaque raster. La méthode ne fournit pas seule une planète sphérique avec raffinement arbitraire, saisons et bilan conservatif. [Article](https://doi.org/10.1145/2461912.2461996).

**Soulèvement et incision.** Cordonnier et al., *Large Scale Terrain Generation from Tectonic Uplift and Fluvial Erosion*, CGF 35(2), 2016, couplent soulèvement tectonique et érosion fluviale. Schott et al., *Large-Scale Terrain Authoring through Interactive Erosion Simulation*, ACM TOG 42(5), 2023, rendent cette famille interactive par approximation du drainage et du routage. Ces travaux motivent une génération globale/régionale des chaînes et bassins avant le détail local. Leur interactivité ne signifie pas génération mondiale métrique en temps réel. [Cordonnier, référence des auteurs](https://perso.liris.cnrs.fr/apeytavi/website/categories/erosion/), [Schott 2023](https://aparis69.github.io/public_html/projects/schott2023_Uplift.html).

**L’érosion multi-échelle est le travail le plus directement aligné avec le raffinement.** Schott et al., *Terrain Amplification using Multi-scale Erosion*, ACM TOG 43(4), 2024, amplifient un terrain grossier par effets thermiques, d’incision et de versant contrôlés à différentes échelles. Étudier cette méthode plutôt qu’empiler bruit et érosion de tuile. Le code officiel, disponible sous Linux, utilise OpenGL compute : référence expérimentale, pas composant WebGPU prêt à intégrer. Permanence stricte des parents et échanges entre tuiles restent des obligations supplémentaires. [Article et résumé des auteurs](https://aparis69.github.io/public_html/projects/schott2024_Erosion.html), [code](https://github.com/H-Schott/MultiScaleErosion).

**Dépressions.** Barnes, Lehman et Mulla, *Priority-Flood*, Computers & Geosciences 62, 2014, proposent remplissage efficace des dépressions et labellisation des bassins sur grilles ou maillages. Utile au diagnostic du drainage, mais remplir toute dépression ne modélise pas les lacs et bassins fermés réels : distinguer ceux à préserver des artefacts. [Article](https://rbarnes.org/sci/2014_depressions.pdf).

<div style="page-break-after: always;"></div>

### Page 2/3 — Représentation, LOD et travaux récents

**Affichage et génération sont deux problèmes distincts.** Losasso et Hoppe (2004), puis Asirvatham et Hoppe dans *GPU Gems 2* (2005), utilisent des geometry clipmaps : grilles imbriquées autour de la caméra, mises à jour incrémentales, détail résiduel et transitions morphées. Cela gère efficacement le rendu, sans créer automatiquement un drainage cohérent. Une cube-sphere à quadtrees fournit l’adressage global ; des clipmaps peuvent compléter l’affichage proche du sol. Ce choix hybride est une piste à mesurer. [Description technique](https://developer.nvidia.com/gpugems/gpugems2/part-i-geometric-complexity/chapter-2-terrain-rendering-using-gpu-based-geometry).

**Le vecteur porte la structure, le raster le calcul local.** Perche et al., *Vector-based Terrain Modelling*, CGF 44(6), 2025, représentent le terrain par primitives vectorielles et convertissent un DEM avec une précision choisie. Huftier et al., *Terrain Synthesis and Authoring based on Iso-Contours*, CGF, 2026, génèrent des courbes de niveau imbriquées, reconstruisent un TIN et permettent la conversion vers un heightfield pour l’érosion. Ils étayent le mélange envisagé : crêtes, côtes et vallées ne doivent pas dépendre exclusivement d’une grille fixe. Ils ne démontrent pas la conservation de flux dans une hiérarchie planétaire. [Perche, code officiel](https://github.com/Arches-Team/vector-based_terrain_modelling), [Huftier, article](https://onlinelibrary.wiley.com/doi/10.1111/cgf.70389).

**Les modèles génératifs planétaires progressent.** Borg et al., *Authoring Terrestrial Planets with Diffusion Models*, CGF, 2026, conditionnent une génération sur globe par altitude, couverture, température, précipitations et réseau fluvial global, avec une représentation quadsphere. Les résultats visent notamment les survols depuis l’exosphère, pas une exploration métrique garantie. Piste pour l’apparence et la diversité ; réduction parent/enfants, bilans et permanence sous raffinement demanderaient des contraintes et validations supplémentaires. [Article](https://onlinelibrary.wiley.com/doi/10.1111/cgf.70390).

**Choix proposé.** Garder un modèle canonique indépendant de la caméra : graphe hiérarchique de bassins/exutoires, primitives de relief et champs géologiques. Produire les rasters comme raffinements contraints, puis les maillages de rendu. Chaque frontière doit être définie par une règle déterministe partagée ; les échanges d’eau et de sédiments doivent être compatibles. Cette synthèse va au-delà du raccordement a posteriori par moyenne des bords, sans constituer déjà un algorithme démontré.

<div style="page-break-after: always;"></div>

### Page 3/3 — Climat, extensions et pistes à départager

**Climat.** Paradise et al., *ExoPlaSim: Extending the Planet Simulator for Exoplanets*, MNRAS, 2022, proposent un modèle de circulation générale de complexité intermédiaire pour des configurations planétaires variées. Référence physique et outil de validation hors de la boucle de rendu. Pour une première approche interactive : bilan énergétique saisonnier et transport simplifié, puis descente d’échelle selon altitude, vents, humidité et orographie. Compromis proposé ; rotations extrêmes et systèmes multi-étoiles doivent être validés séparément. [Article](https://arxiv.org/abs/2107.07685).

**Villes et routes.** Galin et al., *Procedural Generation of Roads*, CGF 29(2), 2010, construisent des chemins selon les coûts du terrain et de construction. Pertinent une fois les implantations fixées : choisir pôles et corridors, puis détailler les tracés sans perdre leurs connexions. Les villes demandent un potentiel — eau durable, fertilité, ressources exploitables, risques, accès aux échanges — et une hypothèse démographique. Cette hiérarchie économique est proposée ici, pas garantie par l’article sur les routes. [Article](https://onlinelibrary.wiley.com/doi/10.1111/j.1467-8659.2009.01612.x).

**Pistes principales.**

| Piste | Intérêt | Limite à résoudre |
|---|---|---|
| Graphe/primitives canoniques + rasters contraints | Objets persistants, calcul local, réemploi du dépôt. | Raffinement des bassins, conservation et côtes stables. |
| Simulation régionale multi-échelle + caches | Relief issu d’érosion ; référence Schott 2024. | Première visite et dépendances amont/aval. |
| Diffusion conditionnée + correction physique | Diversité visuelle ; référence Borg 2026. | Contraintes strictes, coût, données et validation métrique. |

La première piste, complétée par la deuxième pour l’amplification, est la recommandation initiale. Une fonction de bruit seule offre l’accès aléatoire, mais ne porte pas le bilan d’un bassin. Un halo autour d’une tuile améliore le voisinage local, sans fournir tout l’amont d’un fleuve.

**Expérience décisive future, non réalisée ici.** Choisir une région avec côte, fleuve, confluent, lac et partage des eaux. Construire trois niveaux, varier ordre de calcul et cache, mesurer réduction vers le parent, déplacement des objets, continuité, débits et latence. Tester ensuite une frontière entre faces sphériques. Départager les représentations avant villes et routes ; le succès local ne prouve pas toute la planète.

Le verrou reste la synthèse locale rapide sous contraintes globales. Les prototypes fusionnés sont une base d’expérimentation, pas encore une preuve des invariants du monde.

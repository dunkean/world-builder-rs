# Érosion : corrections et limites vérifiées

État du solveur CPU de `world-core`, au 3 octobre 2026. Ces contrôles portent sur
l'atlas physique et ses bilans ; ils ne démontrent pas la qualité visuelle d'une
carte locale de 10 × 10 km.

## Corrections appliquées

- **Base marine à 0 m.** L'incision continentale termine au niveau de l'eau,
  même si le récepteur marin a un fond à −8 000 m. L'ancien choix du fond marin
  creusait artificiellement les cellules côtières.
- **Concentration de l'incision.** L'accumulation amont augmente progressivement
  l'activation du stream power ; les interfluves conservent davantage de relief.
- **Dépôt différé et plafonné.** Une charge excessive ne se dépose plus entièrement
  au premier pixel. Sa fraction déposée dépend d'une longueur de transport et de
  l'espace disponible, ce qui limite les barrages de sédiments et les pics.
- **Transport des berges conservatif.** Une part du matériau adjacent aux drains
  incisés rejoint leur charge ; chaque kilogramme déplacé reste comptabilisé.
- **Petits barrages réellement ouverts.** Les dépressions dont la profondeur au
  déversoir est inférieure à `lake_min_basin_depth_m` sont drainées par excavation
  du corridor. Le nettoyage est répété après le recalage final du niveau marin.
  Les bassins plus profonds restent disponibles pour le calcul des vrais lacs.

Le seuil par défaut de **8 m** est un réglage du prototype. Il ne prouve pas qu'un
étang moins profond est artificiel ; diminuer le seuil conserve davantage de
petits bassins. Aucun lac n'est supprimé uniquement à cause de sa forme ou du
nombre de cellules qu'il occupe.

## Contrôles reproductibles

```sh
cargo test -p world-core erosion::
cargo test -p world-core geological_evolution_changes_terrain_and_conserves_solid_mass
```

Les **6 tests d'érosion passent**, ainsi que le contrôle intégré d'évolution.
Ils vérifient l'intégration implicite sans dépassement, la base marine,
le dépôt progressif, l'ouverture d'un barrage peu profond, la conservation d'un
bassin profond et le bilan d'un barrage alluvial. Le nettoyage alluvial devient
idempotent : un deuxième passage conserve les altitudes bit à bit.

Le bilan suivi est
`roche mobilisée = sédiments stockés + sédiments exportés`. Le soulèvement
est comptabilisé séparément. Les tests imposent une erreur du ledger inférieure
à `1e-10` et un contrôle indépendant depuis les épaisseurs sauvegardées `f32`
inférieur à `1e-6`. Les dépôts cumulés et les cellules incisées peuvent être
comptés plusieurs fois au cours des pas ; ce ne sont pas des stocks finaux.

## Qualité locale et référence Burgmap

La référence active est `city_generator/web/src/gen/terrain/erosion.ts`, en
lecture seule. Son relief local évolue sur une grille de 176 à 224 cellules de
côté, avec une marge de 22 %, puis est rééchantillonné. Sur une carte de 10 km,
la grille de 224 cellules simule donc des pas d'environ **64 m**.

À rayon 1 737 km et atlas `512²` par face, notre espacement moyen équivalent
est **4,91 km** (`sqrt(4πR² / (6N²))`). Les vallées globales peuvent s'améliorer,
mais le bruit fin du rendu ne constitue pas une simulation de ravines à 64 m.
Il reste à faire évoluer des régions canoniques mises en cache, avec marges et
conditions de drainage imposées par le parent, sans recalcul dépendant de la
caméra ni activation brutale au changement de LOD. Cette étape est proposée,
**pas implémentée ici**.

## Fondements et fermetures du prototype

L'incision implicite `n = 1` suit [Braun et Willett (2013)](https://doi.org/10.1016/j.geomorph.2012.10.008).
La distinction transport/dépôt à distance finie s'inspire de
[Davy et Lague (2009)](https://doi.org/10.1029/2008JF001146).
Les coefficients de capacité, longueur de relaxation et transfert des berges
restent des fermetures du prototype ; ce solveur ne reproduit pas intégralement
les modèles de ces publications.

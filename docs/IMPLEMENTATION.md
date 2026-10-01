# État du prototype

Ce document distingue le parcours opérationnel du design de recherche. Le dépôt conserve le cadrage et le design complets dans `docs/` ; les anciens prototypes restent intacts.

## Première tranche

Le compilateur CPU Rust construit un atlas cubique sphérique, une mer, un drainage global, un débit accumulé, des températures et biomes paramétriques. Le service expose les mondes enregistrés et des tuiles calculées à la demande. Le visualiseur est compilé en WebAssembly, avec rendu wgpu dans le navigateur.

La résolution d'atlas définit les grands bassins et les champs climatiques. Une tuile plus fine ne crée pas une nouvelle simulation de climat ou d'histoire géologique. La graine, les paramètres et la version d'algorithme déterminent le monde. Les plaques et la lithologie influencent soulèvement, incision synthétique et détail ; la gravité intervient dans la largeur des chenaux.

Les vues saisonnières utilisent une insolation journalière et une inertie simplifiées, sans changer le terrain ou le graphe. La couche humaine agrarienne place une population sur des sites favorables et relie les sites accessibles par A*. Les composantes insulaires et les connexions impossibles sont signalées ; les bâtiments restent des symboles.

Le mode `generator=contracts` descend réellement depuis la racine de la face dans les enfants déjà raffinés. Il conserve les moyennes de chaque cellule parent dans la quadrature du modèle, les ancres et les frontières quantifiées au centimètre. Les certificats de tous les niveaux accompagnent la tuile. Le mode spatial est conservé pour comparer la qualité du relief et des chenaux. Le visualiseur garde parents et voisins disponibles, avec un cache borné à 160 maillages, mais affiche un seul ensemble complet de tuiles à la fois. Sa couverture s'élargit avec le ratio d'écran. Le serveur limite son cache à 64 Mio et les calculs de tuiles concurrents à quatre.

Le rendu utilise des positions haute/basse précision et une profondeur inversée. La caméra suit les triangles effectivement affichés ; une transition géométrique commune de 350 ms accompagne les changements de niveau. Le LOD affiche une échelle plus fine que le seuil initial et précharge encore le niveau suivant, à 64 segments. Les normales sont lissées et partagées aux frontières du patch. Les indicateurs de surface sont constants par triangle, pour éviter que leur interpolation ne crée des trous. Le relief conserve ses dimensions physiques par défaut (×1) ; l'exagération reste réglable. Le raffinement conservatif utilise la même résolution de grille à tous ses niveaux.

## Limites à garder visibles

- Un lac rempli jusqu'à son seuil n'est pas une simulation de stockage saisonnier ou Fill–Spill–Merge.
- Un bruit spatial reproductible n'est pas une preuve de conservation des moments entre tous les niveaux.
- Les certificats de `world-contracts` portent sur les grilles et leur quadrature ; ils ne certifient ni masse solide ni hydrologie complète. La métrique utilise une aire sphérique au milieu de chaque cellule et des quarts égaux dans la réduction locale, plutôt qu'une intégration exacte de toute la hiérarchie.
- La proposition conservatrice actuelle compense les perturbations d'arêtes au centre des cellules, ce qui peut produire des losanges artificiels. Sa racine sous-échantillonne aussi l'atlas lorsque celui-ci dépasse 64 cellules par face. Ce mode sert à éprouver les certificats ; le mode spatial reste la référence visuelle, jusqu'à une proposition géomorphologique plus riche.
- Le climat et les biomes sont des estimations paramétriques, sans solveur atmosphérique, cycle annuel ni advection conservatrice.
- Le débit cumulé à l'atlas conserve le ruissellement du modèle ; il ne représente pas un calibrage terrestre ou extraterrestre mesuré.
- La 3090 intervient par le renderer du navigateur. L'atlas est calculé sur CPU ; aucun résultat CUDA n'est revendiqué.

## Prochaines extensions utiles

1. Ajouter les contraintes hydrologiques et côtières aux tuiles conservatrices, puis vérifier leurs graphes réduits.
2. Ajouter le stockage des lacs et les profils fluviaux avant d'accroître le détail.
3. Comparer les formes du relief à des références et améliorer l'incision.
4. Introduire climat saisonnier, sédiments et budgets physiques conservatifs.
5. Mesurer le backend GPU de calcul et un visualiseur natif avant de choisir leur priorité.

## Validation

Les 34 tests portent sur géométrie sphérique, drainage, bilan d'eau indépendant, déterminisme, saisons, lithologie, gravité, raccords, réduction parent/enfants, quantification, corruption du stockage, population et routes. Ils vérifient aussi l'intersection caméra/triangles, les normales partagées et la couverture des écrans larges. Le serveur et le WASM compilent en release. Chrome/WebGL2 logiciel vérifie génération, sommets et rivières à 30 km et 1 km, descente à 5 m, six faces, frontière de faces et dézoom rapide. Les captures ne présentent plus les surfaces superposées ou trous du premier renderer ; elles ne constituent pas une validation de réalisme géomorphologique.

Mesure du 1er octobre 2026, générateur `prototype-0.4`, Rust release, CPU de cette machine, compilation exclue : atlas de 98 304 cellules en **498 ms**, 10 000 échantillons en **14,3 ms**, 100 tuiles de 64 segments avec **p50 5,06 ms / p95 6,16 ms**. Ce benchmark mesure le générateur spatial, hors transport JSON et rendu. Commande reproductible : `cargo run --release -p world-core --example benchmark -- 128 42`.

L'API locale vérifiée avec la graine 42 et l'atlas 64 produit un monde de 24 576 cellules, 24 localités et 19 routes, pour 120 000 habitants, sans cycle de drainage ni perte de débit. Le résidu de bilan sur les débits encodés en float32 est d'environ **3,9 × 10⁻⁹** pour cette graine ; les accumulations de référence sont en float64. L'atlas 256 de la graine 1485419110 est également validé : 393 216 cellules, aucun cycle ni récepteur montant, résidu d'environ **2,8 × 10⁻⁹**.

WebGPU s'initialise dans Chrome Linux automatisé, mais sa capture présente un canvas blanc dans cet environnement. Le repli WebGL2 permet la validation visuelle. Ce constat ne qualifie pas WebGPU sur la 3090 : tester Chrome/Edge Windows à même résolution et même trajet avant de conclure sur les performances. Aucune infrastructure CI/CD complexe n'a été ajoutée.

# Qualité locale et référence Burgmap

La référence demandée est le générateur TypeScript actif de
`D:\Workspace\Self\RPG\city_generator\web\src\gen\terrain`, lu sans modification.
Les fichiers Python historiques ne décrivent pas le générateur actuellement utilisé.
Ce document distingue les mécanismes effectivement présents, les mesures CPU et
les éléments encore nécessaires pour atteindre cette qualité sur une région de 10 × 10 km.

## Pourquoi le relief de référence fonctionne

`heightfield.ts` appelle `mountainRelief` dans `erosion.ts` pour les montagnes,
collines et vallées. Le relief vient d'une évolution de paysage : soulèvement
spatial, drainage par priority flood, incision implicite dépendant de l'aire
amont, transport thermique sur les pentes et diffusion. L'évolution compte
18 à 22 itérations sur une grille de 176 à 224 cellules de côté. Une marge de
22 % permet au drainage de dépasser la carte. Le bruit final décore surtout
les versants ; il ne remplace pas les vallées et leurs lignes de partage des eaux.

Sur 10 km, la référence produit une grille finale de 768², soit environ
13 m par cellule ; la grille d'évolution étendue de 224² a un pas de 64 m.
L'atlas planétaire 512² par face du prototype, pour un rayon de 1 737 km,
a un pas moyen équivalent de 4 910 m. Raffiner sa triangulation ne crée donc
pas automatiquement le réseau de petites vallées présent dans Burgmap.

`hydrology.ts` traite ensuite les rivières comme un système cohérent : profil
longitudinal lissé puis décroissant, incision bornée, chenal, berges, plaine
alluviale et versants. Les méandres ont une longueur d'onde liée à la largeur,
une amplitude réduite par la pente et une transition douce aux extrémités.
Le drainage est recalculé après incision. Ces contraintes comptent davantage
que l'ajout de détails aléatoires à une polyligne.

## Ce qui est appliqué au prototype

- L'érosion globale concentre l'incision sur les axes alimentés par l'amont,
  emploie le niveau marin comme base d'érosion et transporte les sédiments
  sur une longueur finie. Les faibles dépressions sont réellement drainées.
- La côte est une composition fixe de torsions sphériques inversibles,
  avec une décroissance fractale d'exposant 0,85 et une coupure par défaut
  à 1 m. Les contrôles hydrologiques sont transportés par la carte inverse.
  Le détail vertical conserve son signe autour du niveau marin. Ni la caméra,
  ni l'ordre des requêtes, ni le niveau de tuile ne choisissent une nouvelle graine.
- La protection du détail près d'un lac dépend continûment des poids de
  l'interpolation ; sélectionner brutalement un lac à la frontière d'un quad
  produisait auparavant des falaises carrées.
- Les réservoirs utilisent le même domaine métrique pour leurs plans d'eau,
  leurs franges sèches et la reconstruction de leurs lignes de partage.
  Les deux propriétaires d'une séparation sont choisis par rayon normalisé ;
  une troisième paire de lacs ne peut pas créer de crête dans leur intérieur.
  Un cache immuable de labels occupe 6 Mio à 512² par face et se partage entre
  clones. Le chemin d'échantillonnage ne consulte que le stencil local,
  sans recherche dans toutes les cellules de tous les bassins.
- Le même mélange continu borne le détail aux vrais contrôles de
  déversement et exutoires secs. Un nœud situé exactement au plan de
  déversement ne peut plus être abaissé par le bruit fin.
- Le catalogue `RiverReach` partage les positions, niveaux, largeurs et
  embouchures entre l'incision locale et la représentation des rivières.
  Les sections utilisent un chenal, des berges, une plaine alluviale et
  des versants raccordés, plutôt qu'une seule parabole plafonnée.

Les patches d'érosion régionale expérimentaux déjà présents restent distincts
du chemin de rendu cohérent utilisé par défaut. Leur présence dans le dépôt
ne signifie pas que toute région explorée bénéficie d'une simulation locale.
Les corrections lacustres reconstruisent du terrain de retenue ; elles ne
constituent pas une nouvelle simulation physique avec bilan de sédiments.
Leur naturalité reste une limite séparée de la continuité numérique.

## Vérification numérique reproductible

```sh
cargo run --release -p world-core --example quality_probe -- 128
cargo run --release -p world-core --example quality_probe -- data/worlds/<id>/atlas.json
cargo run --release -p world-core --example quality_probe -- data/worlds/32ede09abc580c1b3049/atlas.json --overlap-reference=examples/overlap-regression-2386084681.json
```

Le programme emploie la graine 2386084681 lorsqu'il génère un atlas. Il compare
les échantillons complets aux points partagés des tuiles L0 à L24, mesure les
différences de hauteur autour des frontières de support lacustre et examine
des profils au centre des 512 plus grands tronçons fluviaux. La côte d'une
fenêtre de 10 km est aussi échantillonnée sur sept grilles imbriquées,
de 8 à 512 intervalles (1 250 à 19,5 m).
Il n'effectue aucune mesure du GPU ou des images du navigateur.
Il cherche aussi les changements de propriétaire aquatique sur 500 100
positions proches des lacs, puis les localise à moins de 1 mm pour comparer
les **surfaces d'eau**, pas seulement le sol. Un contrôle séparé quantifie
le soulèvement imputable à la reconstruction des réservoirs.

Mesures de l'atlas final `32ede09abc580c1b3049`, graine 2386084681, 512²
par face ; deux parcours commencent sur une côte et au centre de la caméra :

| Contrôle | Résultat |
| --- | ---: |
| Points communs parent/enfant contrôlés, L0→L24 | 1 200 |
| Échantillons complets différents à ces points | 0 |
| Paires aux frontières de support lacustre | 39 750 |
| Différence maximale pour 3,47 cm de pas nominal | 13,9 mm |

Les différences lacustres sont des différences finies incluant la pente du
terrain, pas une mesure de saut discontinu. Les grilles côtières imbriquées
conservent leurs points communs. Un nombre de composantes raster différent
peut néanmoins révéler un chenal sous-échantillonné : cette mesure ne démontre
pas une invariance topologique de tous les polygones à toute résolution.
Dans cette fenêtre côtière, l'écart de classification marine entre les
triangles de 8, 16, 32 et 64 intervalles et la grille de référence de 512
vaut respectivement 3,00 %, 1,02 %, 0,34 % et 0,11 %. Cette mesure affine
ne reproduit ni la projection de la sphère ni le shader du navigateur.

L’analyse initiale a détecté des profils fluviaux trop profonds : 80,96 m
sur l’atlas 128 et 111,64 m sur le 512, contre environ 2 m attendus.
Le catalogue compile maintenant un profil spatial monotone à 128 segments
par reach, ajusté au même terrain naturel. Les niveaux des nœuds sont
partagés et les contraintes propagées sur le graphe ; les contrôles mer/lac
restent fixes. Les reaches s’arrêtent à la première rive d’un réservoir aval.
Le début d'une sortie de lac utilise la dernière rive ascendante avant
son contrôle, pour ne pas confondre un îlot intérieur avec le déversement.
Dans une section mouillée, une vallée sèche voisine ne peut plus creuser
le lit en laissant au-dessus une autre surface d’eau.
Trois passes de catalogue plafonnent les profils sous la surface des autres
vallées au centre et aux bords mouillés. Le propre reach est exclu ; les
surfaces libres remplacent les lits comme borne dans les sections mouillées,
pour éviter une baisse répétée d'une profondeur à chaque passe. Les nœuds
et contrôles restent fixes. Le sampler utilise ensuite une partition
continue des couples lit/profondeur, avec support complet dans le chenal
et décroissance sur les berges ; les propriétaires ne choisissent plus
les hauteurs. Les potentiels signés des berges empêchent une bosse quand
deux sections partagent le même plan d'eau.

Après intégration, le contrôle 512 porte sur **15 872 positions de 512 grands
reaches** : profondeur fluviale maximale **2,6701 m**, conforme à la section
canonique, aucun niveau dépassant le terrain naturel de plus de 10 m,
écart positif maximal 0,9312 m. L'ancienne priorité binaire produisait
des marches réelles de sol de 19,58 m et d'eau de 1,10 m aux confluences.
Le contrôle final reprend les dix voisinages initiaux, même si leurs
anciens mismatches ont disparu : **28 980 points**, **620 frontières de
propriétaire ou de mouillage**, maximum du sol **0,061 mm**, aucun saut
d'eau mesurable à la précision `f32` sur les 106 paires mouillées.
La profondeur locale maximale vaut 2,1408 m. La borne du mélange est la
profondeur maximale des contributeurs ; l'identifiant choisi n'en définit
pas le budget.

Le champ mélangé diffère encore du profil individuel sur 57 des 15 872
positions, maximum 3,3062 m. Le propriétaire y est identique : ce sont
des écarts entre catalogue et reconstruction, pas des sauts de propriétaire.
La partition garantit continuité et profondeur bornée ; elle ne constitue
pas un solveur hydraulique ni une preuve de monotonie du champ final.
Ce contrôle porte sur un monde et les reaches principaux, pas sur
une preuve universelle de tous les chenaux.
Un début de tronçon à l'intérieur d'un réservoir peut légitimement être
profond. Le probe publie séparément la profondeur de tous les plans d'eau
et celle des rivières hors mer et lacs, afin de ne pas confondre ces cas.

Les surfaces aquatiques ont aussi révélé un défaut que la continuité du
sol ne détectait pas : jusqu'à 101,8 m de différence entre deux plans
humides voisins. Le domaine métrique et les contrôles de déversement
suppriment ces grands sauts dans le relevé final. Le backwater adaptatif
des entrées lacustres corrige aussi les raccords latéraux résiduels.
La sonde relève **zéro saut de propriétaire supérieur à 10 cm** et aucune
différence d'eau mesurable à la précision `f32` aux changements examinés
sur les 500 100 positions proches des lacs. Les 1 200 échantillons complets
parent/enfant restent identiques.
Le [rapport de revue lacustre](LAKE_REVIEW.md) expose les corrections et
la limite de naturalité du terrain de retenue ajouté.
Le catalogue froid prend environ 5,43 s pour les caps seuls ; la sonde
finale mesure 9,87 s pendant les compilations native/WASM concurrentes.
L'analyse finale prend 8,89 s sous cette même charge. Ces mesures ne
décrivent pas le coût d'une tuile déjà servie depuis le cache.

## Complément minimal pour la qualité 10 km

L'étape suivante doit simuler une région géographique fixe, pas la tuile
visible : clé spatiale canonique, marge, conditions d'altitude et de pente
héritées du parent, entrées et sorties hydrologiques partagées, puis une
évolution locale à un pas de quelques dizaines de mètres. Les voisins
doivent utiliser les mêmes contrôles aux frontières. Un simple fondu vers
le parent évite un saut de hauteur, mais ne garantit pas que les cours d'eau
des deux régions se rejoignent.

Le résultat doit être calculé et sauvegardé avant publication, servi depuis
un cache borné, puis échantillonné à tous les LOD. Ses petits détails peuvent
être filtrés au rendu ; sa côte, ses vallées et ses connexions hydrologiques
doivent conserver les mêmes coordonnées. Cela ajoute un vrai réseau local
sans déplacer une crique ou un affluent chaque fois que l'utilisateur zoome.
Il ne faut pas présenter le prototype actuel comme équivalent à cette
simulation locale tant que cette étape et ses raccords n'ont pas été mesurés.

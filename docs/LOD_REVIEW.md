# Revue du streaming et des transitions LOD

Revue du 3 octobre 2026, effectuée en CLI par **Claude Opus 5.5**
(`claude-opus-5-5`). Le modèle effectif a été vérifié dans le flux de sortie.
Cette revue lit le code ; elle ne remplace pas les essais WebGPU sur la RTX 3090.
Le périmètre couvre `world-viewer/src/lib.rs`, `lod.rs`, `tile-stream.js` et le
câblage des changements de monde dans `app.js`.

## Corrections apportées après la revue

- **Voisinage périmé après remise à zéro.** `clear_tiles` invalide maintenant le
  cache de voisins et change le numéro du découpage. Une ancienne clé retirée du
  cache ne peut plus être réutilisée comme job de bord après un changement de
  mode de raffinement.
- **Paquets d'une ancienne époque.** `TileStream.reset(epoch)` annule les jobs,
  vide immédiatement les paquets préparés, les résidents connus et le contexte
  de requête. `app.js` l'appelle à chaque changement de monde, saison, méthode de
  raffinement ou exagération. La prochaine frame ne peut plus installer une
  tuile préparée pour le contexte précédent.
- **Annulations bloquant le prochain déplacement.** Les jobs annulés libèrent
  immédiatement leurs trois emplacements. Leurs identifiants uniques permettent
  d'ignorer les réponses tardives sans bloquer la nouvelle demande.
- **Résidence après éviction.** Chaque mise à jour recale `loaded` sur les clés
  réellement présentes dans Rust. Un parent évincé pendant un zoom arrière est
  redemandé, même si aucune nouvelle installation n'a annoncé l'éviction. Les
  paquets préparés devenus redondants sont écartés.
- **Budgets qui s'additionnaient.** Les différents passages de planification et
  de préparation partagent une échéance de frame : 4 ms en déplacement, jusqu'à
  8 ms après 150 ms d'immobilité, modulés par le temps de rendu observé. La
  planification s'arrête aussi sur les candidats rejetés. Un nouveau job lourd
  attend la frame suivante s'il reste moins de 1 ms.
- **Allocations transitoires.** L'installation GPU réutilise la géométrie déjà
  construite pendant la préparation au lieu de la recalculer. L'éviction détruit
  explicitement les buffers WebGPU, sans attendre le ramasse-miettes JavaScript.
- **Quartets rechargés sous pression.** Le cache ne réservait qu'un quartet alors
  que le streaming demandait tous les suivants : leurs premiers paquets pouvaient
  être immédiatement évincés, puis préparés de nouveau. Les nouvelles requêtes
  sous pression se limitent désormais au quartet effectivement réservé. Tous les
  parents nécessaires au zoom arrière sont conservés dans la demande.
- **Latence cumulée des transitions.** Le morph dure maintenant **160 ms**,
  contre 350 ms auparavant. Une seule constante Rust fournit aussi la durée au
  shader WGSL ; le placement, l'easing, les gardes de voisinage et la publication
  atomique restent communs aux deux sens du zoom.

Le passage instantané d'un groupe hors champ vérifie le parent cible, les
enfants et les voisins. Cette visibilité est revérifiée après la préparation :
un déplacement qui expose le groupe déclenche le morph inverse visible.

Les tuiles diffusées utilisent **64 segments**, les racines **128**. Le premier
quartet ne fait que repartitionner exactement les sommets de la racine ; les
niveaux suivants ajoutent du détail. Le niveau cible augmente d'un cran, avec
un plafond de 256 tuiles de frustum : `64 × 2^(L+1) = 128 × 2^L` conserve le
pas physique des sommets. Chaque paquet et buffer est environ quatre fois plus
petit, notamment pour les ancêtres et tuiles périphériques. Le nouveau plafond
corrige aussi le sous-échantillonnage qu'imposait parfois l'ancien plafond de
64 tuiles ; le gain mémoire total dépend donc du point de vue.

## Conclusion sur les batches

La revue confirme que chaque quartet conserve **son propre parent source**. Les
parents sont préparés avant leurs enfants lors du zoom arrière ; les voisins
sont mis à jour ensuite. La nouvelle partition est publiée atomiquement. Les
groupes ayant des sources ou jobs de bord en commun sont exclus du même batch.

Deux empreintes disjointes peuvent se toucher par leur anneau extérieur. La
revue ne démontre pas de défaut dans ce cas ; ajouter un anneau d'exclusion
supplémentaire réduirait inutilement le parallélisme sans bug mesuré.

## Limites et pistes mesurables

Le parcours GPU intermédiaire en 128 segments montrait environ **333–363 ms
par commit** aux zooms 10 km, 200 m et 5 m : la cadence suivait surtout l'attente
de 350 ms. Le [parcours final à 64 segments et 160 ms](PERFORMANCE_0_7.md)
mesure un rAF P95 de 16,8 ms, sans Long Task de plus de 50 ms. Le centre à
5 m atteint son niveau cible en 15,69 s et la file se vide en 16,31 s.
Le rendu reste interactif, mais l'arrivée du détail prend encore plusieurs
secondes ; ces temps ne représentent pas un raffinement instantané.

Les 768 Mio CPU et GPU constituent un objectif d'éviction, **pas une borne
absolue** : une partition active et ses sources ne peuvent être supprimées pour
satisfaire le budget. Les mesures navigateur doivent annoncer cette différence.

Un téléversement terminé reste indivisible. La création initiale des vecteurs et
les appels WebGPU peuvent donc dépasser l'échéance, malgré le découpage du travail
sur les sommets. Les jobs de bord réécrivent encore un buffer dynamique entier ;
un regroupement des sommets de bord réduirait ces copies mais demanderait de
changer le format de préparation.

À mesurer avant d'engager une nouvelle refonte :

- coût de l'adjacence sur une partition froide et très déséquilibrée ;
- éventuelles dépendances de couture indirectes avec un écart de plus de huit
  niveaux entre voisins ;
- réutilisation des 32 slots de morph avant la fin des 160 ms au-delà de 200 Hz ;
- installation principale des paquets préparés et mémoire réellement épinglée.

Ces points restent des hypothèses ou des coûts connus, pas des artefacts
reproduits par la revue. La cible actuelle du budget est une frame de 16 ms.

## Validation

`cargo test -p world-viewer` couvre les partitions aller-retour, les groupes
disjoints, les sources parentes exactes, les grilles racine/enfant 128/64, les
normales de relief au niveau 22 et les requêtes de coarsening sous pression.
`cargo check -p world-viewer --target wasm32-unknown-unknown` vérifie aussi le
code navigateur, absent de la compilation native. Les huit tests de
`node --test web/tile-stream.test.mjs` couvrent notamment l'éviction, le reset
synchrone et les réponses tardives après annulation.

Pour les essais GPU, relever `worldBuilderPerf()` pendant zoom avant/arrière,
déplacement et bascule de raffinement pendant un morph. Vérifier les côtes
avant/après, les trous visibles, les erreurs de rendu, les Long Tasks et les
compteurs de mémoire/installations. Les métriques de préparation ne couvrent pas
à elles seules le décodage JavaScript, l'attente réseau ou le temps GPU. Mesurer
séparément l'arrivée du détail visible et la fin de toute la file : le scénario
de test attend aussi les voisins et requêtes anticipées, et non le seul centre.

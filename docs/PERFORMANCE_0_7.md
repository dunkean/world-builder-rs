# Navigation et cache du prototype 0.7

Mesures du 3 octobre 2026 dans Chrome Windows, WebGPU NVIDIA Ampere/RTX 3090,
fenêtre 1280×900, graine 2386084681, atlas 512² par face. Le parcours emploie
le renderer final à 64 segments, morph 160 ms et worker 128 Mio/384 Mio/2048 entrées.
La révision de surface serveur du parcours était `stable-topology-4`, avant
les dernières corrections des raccords hydrauliques ; il
mesure le streaming et le rendu, pas la qualité physique de ces raccords.

## Parcours mesuré

Les positions côtière et intérieure sont fixes. Chaque changement déplace
immédiatement la caméra ; le programme attend ensuite jusqu’à 25 secondes.
« Centre prêt » signifie que la tuile au centre atteint le niveau demandé.
« File vide » inclut les bordures et préchargements : ce délai ne signifie pas
que l’image reste bloquée. Les niveaux cibles dépendent aussi du frustum.

| Vue | Centre au niveau cible | File vide |
| --- | ---: | ---: |
| Côte, altitude 100 km | 10,39 s | 20,85 s |
| Côte, 10 km | 5,49 s | 13,89 s |
| Côte, 200 m | 8,76 s | 19,36 s |
| Centre initial, 5 m | 15,69 s | 16,31 s |
| Retour côte, 200 m | 5,78 s | 15,16 s |

Sur ce parcours : rAF P95 **16,8 ms**, maximum **33,4 ms**, aucune Long Task
(>50 ms) ni erreur console. L’installation principale maximale est 24,2 ms :
une installation reste indivisible et peut dépasser le budget d’une frame.
Au retour, **4 nouvelles requêtes réseau**, 311 lectures disque et 1 hit RAM
worker ; 314 maillages sont préparés. Le cache de paquets fonctionne, mais le
budget de buffers ne garde pas deux régions profondes entièrement résidentes.

Un déplacement local de 30 m puis 60 m à 200 m d’altitude conserve immédiatement
le niveau 17 : 4 puis 0 requêtes réseau, 4 puis 0 installations. Le retour fait
0 requête réseau et 6 installations depuis les caches. Contrôle après 350 ms ;
cette cadence de mesure ne donne pas une latence précise sous 350 ms.

## Mémoire et gains

Le cache Rust comptabilise environ **1,026 Go CPU / 664 Mo GPU** sur la côte ;
le pic CPU du parcours est 1,032 Go. Les budgets 768 Mio sont souples : les
sources et la partition active restent protégées. Ces compteurs excluent
les réserves WASM, l’aperçu conservé, les paquets worker et le pilote GPU.

Avant réduction des tuiles, la même côte occupait 1,44 Go CPU et le zoom à 5 m
n’avait pas entièrement convergé au bout de 25 s. La sélection 64 segments
atteint toutefois davantage de détail : le précédent plafond de 64 tuiles
visibles sous-échantillonnait le frustum. Ces temps ne sont donc pas une
comparaison à résolution strictement identique.

Les nouveaux plafonds de nombre permettent au cache disque worker d’atteindre
839 entrées dans le test, au lieu de 256. Les octets restent bornés. Sous
pression, les demandes attendent le quartet réellement réservé plutôt que
charger des frères immédiatement évincés. CPU et shader partagent la durée
de morph ; le retour de zoom conserve ses parents indispensables.

## Limite actuelle

Le rendu reste interactif pendant les calculs ; **le raffinement profond
n’est pas instantané**. Les sources géographiques restent fixes et les
sommets communs identiques, mais transitions, raccords et reconstructions de
buffers ajoutent encore plusieurs secondes après un grand saut de caméra.
Ces essais couvrent un monde et un parcours, pas toutes les directions,
inclinaisons, graines ou machines.

## Contrôle visuel après correction hydraulique

La révision serveur `spherical-coast-6` a ensuite été parcourue sur le même
GPU : globe à 7R, côte à 1 000 km puis 200 m, centre initial à 5 m et retour
au globe. Les centres atteignent respectivement L2, L4, L17, L22 et L2 ;
les cinq vues ont zéro fallback, sans erreur de page. Les délais observés
sont 2,98 s, 6,45 s, 16,06 s, 12,51 s et 7,49 s, comprenant une attente
de 750 ms avant capture. Des requêtes périphériques sont reparties après
cette attente dans les deux vues côtières : ce contrôle vérifie le centre
et les images, pas une file définitivement vide. Ce parcours ne remesure
pas les percentiles rAF du premier benchmark.

Captures : [premiers niveaux](screenshots/coherent-0.7-early.png),
[centre initial à 5 m](screenshots/coherent-0.7-initial-center.png),
[raccord fluvial corrigé](screenshots/confluence-after-0.7.png). Les surfaces
restent opaques dans ces vues ; la marche noire du raccord fluvial n’est plus
visible. Les matériaux et la qualité de l’érosion locale restent à améliorer.

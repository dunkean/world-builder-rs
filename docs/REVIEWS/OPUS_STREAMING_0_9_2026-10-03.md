# Revue Opus du préchargement — 3 octobre 2026

Revue en lecture seule par `claude-opus-5-5`, via Claude CLI, sur le
streaming qui précédait le correctif. Les constats ci-dessous sont un audit
de source, pas des mesures de navigateur. La variante retenue projette
un seul étage **demandé** dans une coupe virtuelle, sans attendre sa
résidence ; la variante à point fixe résident reste une proposition.
La frontière réellement nécessaire conserve sa priorité, les extras
sont bornés, et un créneau réseau reste disponible pour le terrain visible.
Les contrôles exécutés et leurs limites figurent dans
[le rapport de rendu](../RENDERING_0_9.md).

---

# Revue du streaming en zoom rapide (lecture seule, rien n'a été exécuté)

## Causes prouvées

1. **La prédiction re-demande la génération que l'on charge déjà.**
   - `requests_at` (`crates/world-viewer/src/lib.rs:2285`) appelle `frontier_keys(&self.active_set, …)`. Cette fonction (`:1052`) ne renvoie que les enfants des feuilles **actives** visibles.
   - Avec un cut actif en L5, `requests()` (`:2307`) renvoie les enfants L6. `requests_at(alt*0.5)` renvoie aussi des enfants L6 : changer de cible ou d'altitude ne change que le filtre `visible`, jamais la profondeur.
   - Le `seen.insert` les supprime presque tous. `prediction_requests` (`:3872`), utilisé par `app.js:215`, a le même défaut.
   - Résultat : il n'y a aucun vrai préchargement d'une échelle plus fine.

2. **La chaîne de chargement est strictement sérielle, une génération à la fois.** On ne demande L(n+2) qu'après le *commit* de L(n+1) dans `active_set` (`:2948`). Ce commit exige que :
   - les 4 enfants soient résidents et préparés (`:2908`) ;
   - `plan_transitions` les accepte : 8 parents maximum par lot, 2 ms de planification, empreintes disjointes, `neighbors_settled` (`:2664`) ;
   - `process_cut` ait terminé avec un budget de 4 ms par image pendant le mouvement (`:42`) ;
   - le parent ait fini son propre morph de 0,16 s avant de se re-diviser (`:2913`).

   Passer de L5 à L10 demande donc 5 allers-retours réseau en série, chacun suivi de ce pipeline de commit.

3. **Côté JS, les requêtes ne sont pas hiérarchisées, mais ce n'est pas encore la cause.**
   - `app.js:207` traite `info.requests` comme entièrement essentiel, alors qu'il contient déjà la partie spéculative.
   - `tile-stream.js:57` remplit les 3 slots dans l'ordre, sans réserve ni préemption.
   - Comme la part spéculative est aujourd'hui dupliquée, cela ne bloque rien maintenant. Ça bloquera dès qu'un vrai préchargement existera.

4. **Ce qui n'est pas une cause démontrée :**
   - Les clés de coarsening placées en tête : les parents des feuilles actives sont épinglés (`:2516`), donc presque toujours déjà résidents et filtrés.
   - L'absence d'équilibrage 2:1 : `lod::split` n'en impose aucun.

## Votre proposition : sûre, et elle fait réellement avancer d'une échelle

- **Sûre :** elle ne touche ni la géométrie ni le cut réel. `include_coarsen=false` pour les extras. La branche sous pression mémoire (`pressure_frontier`) reste calculée sur le cut **réel**.
- **Gain réel :** elle donne une génération d'avance. L(n+2) se télécharge pendant le commit et le morph de L(n+1).

**Variante plus petite et meilleure : un point fixe sur la résidence.**
- On divise de façon répétée, dans un cut virtuel, chaque feuille visible de niveau `< level` dont les 4 enfants sont dans `tile_lookup`.
- On ne demande jamais une tuile déjà résidente, ni de l'« historique ». On ne demande qu'**une** génération non résidente au-delà de la frontière résidente.
- Le préchargement suit alors le débit réseau au lieu du rythme des commits. Le code reste à peu près le même.

**Patch minimal recommandé :**
- **Rust :**
  - Ajouter une fonction libre près de `frontier_keys`, pour qu'elle soit testable hors du module wasm `browser` : `prefetch_keys(cut, resident: impl Fn(TileKey)->bool, targets, level, direction) -> Vec<TileKey>`.
    1. Cloner `cut`.
    2. En boucle : appliquer `lod::split` aux feuilles visibles dont le quartet est résident, jusqu'à ce que plus rien ne change.
    3. Appeler `frontier_keys(&virtual, targets, level, direction, false)`.
  - Dans `requests_at`, utiliser `prefetch_keys` avec `|k| self.tile_lookup.contains_key(&k)`. `prediction_requests` en profite automatiquement.
  - Dans `requests()`, garder la frontière réelle en premier, puis ajouter les extras avec `.take(64)`. Enregistrer la longueur essentielle avant d'ajouter les extras.
  - Dans `view_info`, exposer `"essential_count": n` plutôt qu'un second tableau : le JSON est déjà lourd avec `resident_keys`, et il est produit toutes les 50 ms.
- **JS :**
  - `app.js` : `critical = new Set(info.requests.slice(0, info.essential_count ?? Infinity).map(keyOf))`, passé dans le contexte.
  - `tile-stream.js`, dans `dispatch` : si `context.critical` existe, une clé non critique fait `continue` dès que 2 jobs non critiques sont déjà en vol.
  - Ne rien annuler de plus : l'annulation reste réservée aux clés hors de `desired`. Sans `critical`, le comportement reste celui d'aujourd'hui.

**Cas limites :**
- **Cut équilibré :** pas de contrainte 2:1, donc rien à faire. Les voisins ne jouent que sur le timing.
- **Coarsening et zoom arrière :** `level` est inférieur au niveau des feuilles, donc aucun extra. L'ordre des clés de coarsening ne change pas.
- **Pression mémoire :**
  - Au-dessus de 80 %, les extras sont déjà coupés.
  - Entre 80 et 100 %, des petits-enfants résidents mais non commités ne sont ni dans `wanted` ni épinglés. Ils seraient évincés en premier. C'est acceptable.
  - Ne **pas** ajouter de pin.
- **Démarrage :** `active_set` vide donne un cut virtuel vide, donc aucune requête.
- **Pins du cache :** inchangés. Les racines L0 sont déjà exclues de l'éviction.
- **Zoom rapide :**
  - Les jobs spéculatifs restent valides tant que leur ancêtre reste visible.
  - Un risque mineur : à la limite du `take(64)`, une clé peut entrer et sortir de la liste et être annulée puis relancée. Le tri par niveau puis par direction limite ce va-et-vient.
  - `app.js:208` ne teste que la mémoire CPU, pas la GPU (incohérence mineure).

## Tests de régression requis

**Rust** (en partant de `:4939`) :
- Cut L5 sur le chemin de la cible, quartet L6 résident : on obtient les clés L6 essentielles d'abord, puis L7 sous le quartet, et rien en L8.
- Quartet résident à 3/4 seulement : aucun petit-enfant.
- Quartets L6 et L7 résidents : on demande L8, jamais L6 ni L7 à nouveau, jamais au-delà de `level`.
- Parent non visible avec un quartet complet : il n'est pas étendu.
- Zoom arrière : aucun extra, et l'ordre de coarsening reste identique.
- Borne de 64, extras toujours placés après les essentiels.
- Les tests `pressure_frontier` existants (`:5009`) ne changent pas.

**JS** (`web/tile-stream.test.mjs`) :
- Avec `critical`, au plus 2 jobs non critiques en vol. Une clé critique qui arrive plus tard part sans annuler un job spéculatif encore désiré.
- Sans `critical` : comportement actuel.
- Une repriorisation n'annule pas un job spéculatif encore présent dans `desired`.

## Limites inévitables d'un saut à froid

- Même avec le point fixe, un cache froid L5 → L10 coûte toujours 5 allers-retours en série. Chaque génération dépend de la résidence de la précédente, et demander directement L10 violerait la règle « usable partition ».
- Le débit est plafonné par les 3 slots et le temps de génération serveur de chaque tuile.
- Même si tout est en cache, il faut au moins 5 × 0,16 s de morph, plus les lots de 8 parents et le budget de 4 ms par image.
- La prédiction ne voit pas au-delà de l'événement de molette en cours (l'heuristique `alt*0.5`).
- Les « mares polygonales » du fjord viennent de l'échantillonnage de la tuile L5 à 64 segments : le fjord est plus étroit que l'espacement des sommets. Le patch raccourcit leur durée d'affichage, il ne les supprime pas. Seule une autre représentation de l'eau sur les tuiles grossières le ferait, ce qui sort du périmètre.

// Text alternatives describe data availability separately from displayed mesh.
const number = (value, digits = 1) => Number(value).toLocaleString('fr-FR', { maximumFractionDigits: digits });

export function terrainStatus(hybrid, spacing, proceduralSpacing) {
  const resolution = `Maille terrain : ${number(spacing, 3)} m · ${hybrid?.mode === 'hybrid' ? 'source NN native30m, interpolation au-delà' : `bruit original jusqu’à ${number(proceduralSpacing)} m`}`;
  if (!hybrid || hybrid.mode === 'procedural') {
    return `${hybrid?.applied_mode === 'hybrid' ? 'Retour au procédural demandé ; dernier champ NN conservé pendant la transition.' : 'Terrain procédural affiché.'} ${resolution}`;
  }
  if (!hybrid.desired) return `Hybride sélectionné. Vue globale procédurale : aucun détail NN demandé à cette échelle. Zoomer ou choisir « Voir relief NN ». ${resolution}`;
  let state = hybrid.applied_wanted > 0
    ? `Champ NN validé pour ${hybrid.applied_wanted}/${hybrid.desired} patches de cette vue ; le résumé au centre mesure son relief.`
    : 'Terrain procédural affiché dans cette vue ; le champ NN est en préparation.';
  state += ` Données NN (aperçus puis natif30m) : ${hybrid.ready_wanted}/${hybrid.desired} patches chargés${hybrid.pending ? `, ${hybrid.pending} en calcul` : ''}. ${resolution}`;
  if (hybrid.surface_wait_state === 'memory') state += ' Mise à jour en attente de mémoire ; terrain précédent conservé.';
  else if (hybrid.updating) state += ' Mise à jour du maillage en cours.';
  if (hybrid.last_error) state += ' Dernière requête NN en erreur ; terrain précédent conservé.';
  return state;
}

export function reliefSummary(probe) {
  if (!probe?.triangle?.length || !Number.isFinite(probe.displayed_height_m)) return 'Mesure au centre en attente du maillage.';
  const vertices = probe.triangle;
  const deltas = vertices.map((vertex) => vertex.learned_delta_m);
  const slopes = vertices.map((vertex) => vertex.physical_slope_degrees);
  const range = (values, unit) => values.every(Number.isFinite)
    ? `${number(Math.min(...values))} à ${number(Math.max(...values))} ${unit}` : 'indisponible';
  const complete = vertices.every((vertex) => vertex.morph_progress >= 0.999);
  const direction = probe.direction;
  const location = direction?.every(Number.isFinite)
    ? ` · ${number(Math.asin(Math.max(-1, Math.min(1, direction[1]))) * 180 / Math.PI, 3)}° lat., ${number(Math.atan2(direction[0], direction[2]) * 180 / Math.PI, 3)}° lon.` : '';
  return `Centre${location} : altitude ${number(probe.displayed_height_m)} m. Pente des sommets : ${range(slopes, '°')}. Δ NN ${complete ? 'appliqué' : 'cible, transition en cours'} : ${range(deltas, 'm')}.`;
}

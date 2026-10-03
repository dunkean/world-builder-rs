// Generation settings use API units internally; only the form uses display units.
export const DEFAULT_CONFIG = Object.freeze({
  seed: 42,
  radius_m: 1737000,
  resolution: 128,
  ocean_fraction: 0.58,
  relief_m: 6000,
  detail_m: 140,
  coast_detail_ratio: 0.16,
  coast_reference_spacing_m: 1,
  lake_min_basin_depth_m: 8,
  axial_tilt_deg: 23.4,
  gravity_m_s2: 3,
  water_volume_m3: null,
  erosion_steps: 24,
  geological_years: 2000000,
  uplift_m_per_year: 0.00015,
  stream_power_k_per_year: 0.000008,
  hillslope_diffusivity_m2_per_year: 0.8,
  lake_fill_years: 25,
});

export const PRESETS = Object.freeze({
  balanced: { label: 'Équilibré', description: 'Atlas 128, relief de 6 km et 24 étapes d’érosion.', config: {} },
  preview: { label: 'Essai rapide', description: 'Atlas 64 et 12 étapes : moins de calcul global pour choisir une graine.', config: { resolution: 64, erosion_steps: 12 } },
  mountains: { label: 'Relief marqué', description: 'Relief de 10 km, davantage de terres et 48 étapes d’érosion.', config: { relief_m: 10000, detail_m: 220, ocean_fraction: 0.45, erosion_steps: 48, uplift_m_per_year: 0.0002 } },
  oceanic: { label: 'Monde océanique', description: '82 % d’océan visé, relief de 3,5 km et 32 étapes d’érosion.', config: { ocean_fraction: 0.82, relief_m: 3500, detail_m: 90, erosion_steps: 32 } },
});

const fields = [
  ['seed', 'seed', 1], ['resolution', 'resolution', 1], ['radius_m', 'radius', 1000],
  ['ocean_fraction', 'ocean-fraction', 0.01], ['relief_m', 'relief', 1], ['detail_m', 'detail', 1],
  ['coast_detail_ratio', 'coast-detail', 0.01], ['coast_reference_spacing_m', 'coast-spacing', 1],
  ['lake_min_basin_depth_m', 'lake-depth', 1], ['axial_tilt_deg', 'tilt', 1],
  ['gravity_m_s2', 'gravity', 1], ['erosion_steps', 'erosion-steps', 1],
  ['geological_years', 'geological-years', 1000000], ['uplift_m_per_year', 'uplift', 0.001],
  ['stream_power_k_per_year', 'stream-power', 0.000001],
  ['hillslope_diffusivity_m2_per_year', 'diffusivity', 1], ['lake_fill_years', 'lake-fill', 1],
];

const constraints = [
  ['radius_m', 'Le rayon (m)', 10000, 100000000],
  ['ocean_fraction', 'La fraction océanique', 0.05, 0.95],
  ['coast_detail_ratio', 'Le détail côtier', 0, 1],
  ['coast_reference_spacing_m', 'L’échelle des côtes (m)', 0.25, 10000],
  ['lake_min_basin_depth_m', 'La profondeur minimale des bassins (m)', 0, 500],
  ['axial_tilt_deg', 'L’inclinaison axiale (°)', 0, 90],
  ['gravity_m_s2', 'La gravité (m/s²)', 0.05, 100],
  ['geological_years', 'La durée géologique (ans)', 1, 100000000],
  ['uplift_m_per_year', 'Le soulèvement (m/an)', 0, 0.01],
  ['stream_power_k_per_year', 'Le coefficient d’incision (/an)', 0, 0.01],
  ['hillslope_diffusivity_m2_per_year', 'La diffusion (m²/an)', 0, 1000],
  ['lake_fill_years', 'La durée de remplissage (ans)', 0.01, 10000],
];

export function normalizeConfig(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('La configuration doit être un objet JSON.');
  const source = value.config ?? value;
  if (!source || typeof source !== 'object' || Array.isArray(source)) throw new Error('Le champ config doit être un objet JSON.');
  const config = { ...DEFAULT_CONFIG, ...source };
  if (!Number.isSafeInteger(config.seed) || config.seed < 0) throw new Error('La graine doit être un entier entre 0 et 9 007 199 254 740 991 pour cette interface web.');
  if (!Number.isInteger(config.resolution) || config.resolution < 8 || config.resolution > 1024) throw new Error('La résolution de l’atlas doit être un entier entre 8 et 1024.');
  if (!Number.isInteger(config.erosion_steps) || config.erosion_steps < 0 || config.erosion_steps > 256) throw new Error('Le nombre d’étapes doit être un entier entre 0 et 256.');
  for (const [key, label, min, max] of constraints) {
    if (!Number.isFinite(config[key]) || config[key] < min || config[key] > max) throw new Error(`${label} doit être compris entre ${min} et ${max}.`);
  }
  if (!Number.isFinite(config.relief_m) || config.relief_m < 10 || config.relief_m > config.radius_m * 0.08) throw new Error('Le relief doit être d’au moins 10 m et ne pas dépasser 8 % du rayon.');
  if (!Number.isFinite(config.detail_m) || config.detail_m < 0 || config.detail_m > config.relief_m) throw new Error('Le détail du terrain doit être compris entre 0 et l’amplitude du relief.');
  if (config.water_volume_m3 !== null && (!Number.isFinite(config.water_volume_m3) || config.water_volume_m3 <= 0)) throw new Error('Le volume d’eau imposé doit être strictement positif.');
  return config;
}

export function presetConfig(name, seed = DEFAULT_CONFIG.seed) {
  if (!PRESETS[name]) throw new Error('Préréglage inconnu.');
  return normalizeConfig({ ...DEFAULT_CONFIG, ...PRESETS[name].config, seed });
}

export function configurationValues(config) {
  const values = {};
  for (const [key, id, factor] of fields) values[id] = String(config[key] / factor);
  values['water-mode'] = config.water_volume_m3 === null ? 'fraction' : 'volume';
  values['water-volume'] = String(config.water_volume_m3 === null ? 1000000 : config.water_volume_m3 / 1e9);
  return values;
}

function numericValue(value, id) {
  if (typeof value !== 'string' || !value.trim()) throw new Error(`Le champ « ${id} » doit contenir un nombre.`);
  const result = Number(value);
  if (!Number.isFinite(result)) throw new Error(`Le champ « ${id} » doit contenir un nombre fini.`);
  return result;
}

// Preserve the original API number when a control was not edited. Range controls
// may round imported percentages; unchanged configurations must still round-trip.
export function configurationFromValues(values, base = DEFAULT_CONFIG, originalValues = {}) {
  const config = { ...base };
  for (const [key, id, factor] of fields) {
    config[key] = values[id] === originalValues[id] ? base[key] : numericValue(values[id], id) * factor;
  }
  if (values['water-mode'] === 'fraction') config.water_volume_m3 = null;
  else if (values['water-mode'] === 'volume') {
    config.water_volume_m3 = base.water_volume_m3 !== null && values['water-volume'] === originalValues['water-volume']
      ? base.water_volume_m3 : numericValue(values['water-volume'], 'water-volume') * 1e9;
  } else throw new Error('Le mode de quantité d’eau est invalide.');
  return normalizeConfig(config);
}

function signature(config) {
  return JSON.stringify(Object.fromEntries(Object.keys(config).sort().map((key) => [key, config[key]])));
}

export function setupSettings(document, onError = () => {}) {
  const $ = (id) => document.getElementById(id);
  const form = $('generator');
  let base = { ...DEFAULT_CONFIG };
  let originalValues = {};
  let appliedSignature;
  let busy = false;
  const format = (value, digits = 1) => Number(value).toLocaleString('fr-FR', { maximumFractionDigits: digits });
  const currentValues = () => Object.fromEntries([...fields.map(([, id]) => id), 'water-mode', 'water-volume'].map((id) => [id, $(id).value]));
  const read = () => configurationFromValues(currentValues(), base, originalValues);

  function refresh() {
    const volume = $('water-mode').value === 'volume';
    $('ocean-control').hidden = volume;
    $('volume-control').hidden = !volume;
    $('ocean-fraction').disabled = busy || volume;
    $('water-volume').disabled = busy || !volume;
    $('water-volume').required = volume;
    $('ocean-value').value = `${format($('ocean-fraction').value)} %`;
    $('coast-detail-value').value = `${format($('coast-detail').value)} %`;
    $('config-cost').textContent = `${format(6 * Number($('resolution').value) ** 2, 0)} cellules d’atlas · le coût augmente avec la résolution`;
    $('relief').max = String(Number($('radius').value) * 1000 * 0.08);
    $('detail').max = $('relief').value;
    let state;
    let dirty = false;
    try {
      const config = read();
      dirty = appliedSignature !== undefined && signature(config) !== appliedSignature;
      state = appliedSignature === undefined ? 'Réglages prêts à générer' : dirty ? 'Modifications · régénération requise' : 'Réglages appliqués au monde actif';
    } catch {
      state = 'Paramètre à corriger avant génération';
      dirty = true;
    }
    $('config-state').textContent = busy ? 'Génération en cours…' : state;
    $('config-state').classList.toggle('dirty', dirty);
  }

  function apply(value, { applied = false, preset = 'custom' } = {}) {
    base = normalizeConfig(value);
    const values = configurationValues(base);
    if (![...$('resolution').options].some((option) => option.value === values.resolution)) {
      const option = document.createElement('option');
      option.value = values.resolution;
      option.textContent = `${values.resolution} × ${values.resolution}`;
      $('resolution').add(option);
    }
    for (const [id, value] of Object.entries(values)) $(id).value = value;
    originalValues = currentValues();
    $('preset').value = preset;
    $('preset-description').textContent = PRESETS[preset]?.description ?? 'Les réglages prennent effet à la prochaine génération.';
    if (applied) appliedSignature = signature(base);
    refresh();
  }

  function setBusy(value) {
    busy = value;
    for (const control of form.querySelectorAll('input, select, button')) control.disabled = value;
    refresh();
  }

  form.addEventListener('input', (event) => {
    if (event.target.id === 'preset' || event.target.id === 'config-file') return;
    $('preset').value = 'custom';
    $('preset-description').textContent = 'Les réglages prennent effet à la prochaine génération.';
    refresh();
  });
  form.addEventListener('change', refresh);
  form.addEventListener('invalid', (event) => {
    const section = event.target.closest('details');
    if (section) section.open = true;
  }, true);
  $('preset').addEventListener('change', () => {
    if ($('preset').value === 'custom') return;
    try { apply(presetConfig($('preset').value, Number($('seed').value)), { preset: $('preset').value }); }
    catch (error) { onError(error.message); }
  });
  $('randomize').addEventListener('click', () => {
    $('seed').value = globalThis.crypto.getRandomValues(new Uint32Array(1))[0];
    $('preset').value = 'custom';
    refresh();
  });
  $('restore-defaults').addEventListener('click', () => {
    const seed = Number($('seed').value);
    apply({ ...DEFAULT_CONFIG, seed: Number.isSafeInteger(seed) && seed >= 0 ? seed : DEFAULT_CONFIG.seed });
    onError('');
  });
  $('import-config').addEventListener('click', () => $('config-file').click());
  $('config-file').addEventListener('change', async () => {
    const file = $('config-file').files?.[0];
    if (!file) return;
    try {
      if (file.size > 1024 * 1024) throw new Error('Le fichier dépasse 1 Mo. Importez une configuration, plutôt qu’un atlas complet.');
      apply(JSON.parse(await file.text()));
      onError('');
    } catch (error) { onError(`Import : ${error.message}`); }
    finally { $('config-file').value = ''; }
  });
  $('export-config').addEventListener('click', () => {
    try {
      const config = read();
      const url = URL.createObjectURL(new Blob([`${JSON.stringify(config, null, 2)}\n`], { type: 'application/json' }));
      const link = document.createElement('a');
      link.href = url;
      link.download = `planet-${config.seed}.json`;
      link.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      onError('');
    } catch (error) { onError(`Export : ${error.message}`); }
  });

  const tabs = [...document.querySelectorAll('[data-tab]')];
  function selectTab(tab) {
    for (const button of tabs) {
      const selected = button === tab;
      button.setAttribute('aria-selected', String(selected));
      button.tabIndex = selected ? 0 : -1;
      $(`pane-${button.dataset.tab}`).hidden = !selected;
    }
  }
  for (const tab of tabs) {
    tab.addEventListener('click', () => selectTab(tab));
    tab.addEventListener('keydown', (event) => {
      if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
      event.preventDefault();
      const index = event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1
        : (tabs.indexOf(tab) + (event.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length;
      selectTab(tabs[index]);
      tabs[index].focus();
    });
  }
  $('toggle-panel').addEventListener('click', () => {
    const collapsed = document.body.classList.toggle('sidebar-collapsed');
    $('toggle-panel').textContent = collapsed ? '›' : '‹';
    $('toggle-panel').setAttribute('aria-expanded', String(!collapsed));
    $('toggle-panel').setAttribute('aria-label', collapsed ? 'Ouvrir le panneau' : 'Réduire le panneau');
  });

  apply(DEFAULT_CONFIG, { preset: 'balanced' });
  return { read, apply, setBusy };
}

import assert from 'node:assert/strict';
import test from 'node:test';
import { DEFAULT_CONFIG, normalizeConfig, presetConfig, configurationValues, configurationFromValues } from './settings.js';

test('older and wrapped configuration imports acquire new physical defaults', () => {
  const config = normalizeConfig({ config: { seed: 2386084681, resolution: 512 } });
  assert.equal(config.seed, 2386084681);
  assert.equal(config.resolution, 512);
  assert.equal(config.coast_detail_ratio, 0.30);
  assert.equal(config.coast_reference_spacing_m, 0.25);
  assert.equal(config.lake_min_basin_depth_m, 8);
  assert.equal(config.water_volume_m3, null);
  assert.equal(normalizeConfig({ coast_reference_spacing_m: 32 }).coast_reference_spacing_m, 32);
});

test('display units convert to physical API units', () => {
  const values = configurationValues(DEFAULT_CONFIG);
  Object.assign(values, { radius: '2000', 'geological-years': '3.5', uplift: '0.25', 'stream-power': '12', 'coast-detail': '20', 'ocean-fraction': '61' });
  const config = configurationFromValues(values);
  assert.equal(config.radius_m, 2000000);
  assert.equal(config.geological_years, 3500000);
  assert.equal(config.uplift_m_per_year, 0.00025);
  assert.equal(config.stream_power_k_per_year, 0.000012);
  assert.equal(config.coast_detail_ratio, 0.2);
  assert.equal(config.ocean_fraction, 0.61);
});

test('unchanged controls preserve exact imported values and unknown configuration fields', () => {
  const config = normalizeConfig({
    ocean_fraction: 0.5834567812345678,
    coast_detail_ratio: 0.1634567891234567,
    radius_m: 1737000.012345678,
    water_volume_m3: 9.123456789123456e15,
    future_parameter: { mode: 'preserve' },
  });
  const rendered = configurationValues(config);
  // HTML range inputs round to their steps before the snapshot is recorded.
  rendered['ocean-fraction'] = '58.3';
  rendered['coast-detail'] = '16.3';
  assert.deepEqual(configurationFromValues(rendered, config, { ...rendered }), config);
  const edited = { ...rendered, detail: '180' };
  const result = configurationFromValues(edited, config, rendered);
  assert.equal(result.detail_m, 180);
  assert.equal(result.ocean_fraction, config.ocean_fraction);
  assert.equal(result.water_volume_m3, config.water_volume_m3);
  assert.deepEqual(result.future_parameter, config.future_parameter);
});

test('water volume overrides the surface target and can be disabled explicitly', () => {
  const values = configurationValues(DEFAULT_CONFIG);
  values['water-mode'] = 'volume';
  values['water-volume'] = '1234.5';
  const volume = configurationFromValues(values);
  assert.equal(volume.water_volume_m3, 1234500000000);
  const fractional = configurationFromValues({ ...values, 'water-mode': 'fraction' }, volume);
  assert.equal(fractional.water_volume_m3, null);
  assert.throws(() => configurationFromValues({ ...values, 'water-volume': '0' }), /strictement positif/);
});

test('invalid physical combinations fail before a generation request', () => {
  assert.throws(() => normalizeConfig({ radius_m: 10000, relief_m: 801 }), /8 %/);
  assert.throws(() => normalizeConfig({ relief_m: 50, detail_m: 60 }), /amplitude du relief/);
  assert.throws(() => normalizeConfig({ lake_min_basin_depth_m: 501 }), /profondeur/);
  assert.throws(() => normalizeConfig({ coast_reference_spacing_m: 0.1 }), /échelle/);
  assert.throws(() => normalizeConfig({ seed: 9007199254740992 }), /graine/);
  assert.throws(() => normalizeConfig({ resolution: 128.5 }), /résolution/);
  assert.throws(() => normalizeConfig({ uplift_m_per_year: Infinity }), /soulèvement/);
  const values = configurationValues(DEFAULT_CONFIG);
  assert.throws(() => configurationFromValues({ ...values, radius: '' }), /nombre/);
});

test('presets remain valid and preserve the chosen seed', () => {
  for (const name of ['balanced', 'preview', 'mountains', 'oceanic']) {
    const config = presetConfig(name, 2386084681);
    assert.equal(config.seed, 2386084681);
    assert.deepEqual(normalizeConfig(config), config);
  }
  assert.equal(presetConfig('preview').resolution, 64);
  assert.equal(presetConfig('oceanic').water_volume_m3, null);
});

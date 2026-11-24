import { describe, expect, it } from 'vitest';
import fixture from './config-capability-v1.fixtures.json';
import { CONFIG_CAPABILITY_VERSION, configCapability } from './config-capability-v1';
import { buildCandidates, writableSelected } from './batch';
import { createWizard, reduceWizard } from './flow';

describe('config capability v1', () => {
  it('rejects unknown and malformed placeholders', () => {
    for (const value of ['<secret>', '<apiKey><unterminated']) {
      const profile = structuredClone(fixture.profiles.aider);
      expect(configCapability(profile, 'aider', 'openai', 'macos')).toBe('assisted');
      profile.modelProviders.openai.configWrite.fields[0].value = value;
      expect(configCapability(profile, 'aider', 'openai', 'macos')).toBe('manual');
    }
  });

  it('matches the fixture version', () => expect(CONFIG_CAPABILITY_VERSION).toBe(fixture.version));

  for (const { appId, provider, platform, expected } of fixture.cases) {
    it(`${appId}/${provider}/${platform}: ${expected}`, () => {
      const profile = fixture.profiles[appId as keyof typeof fixture.profiles];
      const capability = configCapability(profile, appId, provider, platform);
      expect(capability).toBe(expected);
      const candidates = buildCandidates([{ appId }], fixture.profiles, provider, platform);
      expect(writableSelected(candidates, [appId])).toHaveLength(expected === 'assisted' ? 1 : 0);
      if (provider !== 'missing') {
        const wizard = reduceWizard(createWizard(), { type: 'PICK', providerId: provider, capability });
        expect(wizard.step).toBe(expected === 'assisted' ? 'C2' : 'MANUAL');
        if (expected !== 'assisted') expect(reduceWizard(wizard, { type: 'NEXT' }).step).toBe('MANUAL');
      }
    });
  }
});

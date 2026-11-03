'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { EFFORTS, parseModelReasoning, attachModelReasoning } = require('../modules/modelReasoningCapabilities');

test('names and supported parameter names alone do not prove effort values', () => {
    for (const model of [null, 'gpt', [], {}, { id: 'reasoning-high' },
        { supported_parameters: ['reasoning', 'reasoning_effort'] }]) {
        assert.equal(parseModelReasoning(model).status, 'unknown');
    }
});

test('explicit efforts are filtered, deduplicated and do not mutate catalog', () => {
    const model = { reasoning: { supported_efforts: ['high', 'alien', 'low', 'high'], default_effort: 'low', default_enabled: false } };
    const before = JSON.stringify(model);
    const result = parseModelReasoning(model);
    assert.deepEqual(result.efforts, ['high', 'low']);
    assert.equal(result.status, 'supported');
    assert.equal(result.defaultEffort, 'low');
    assert.equal(result.defaultEnabled, false);
    assert.equal(result.canDisable, null);
    assert.equal(JSON.stringify(model), before);
});

test('null effort allowlist accepts gateway values, mandatory prohibits none', () => {
    assert.deepEqual(parseModelReasoning({ reasoning: { supported_efforts: null } }).efforts, EFFORTS);
    const result = parseModelReasoning({ reasoning: { supported_efforts: null, mandatory: true } });
    assert.equal(result.canDisable, false);
    assert.equal(result.efforts.includes('none'), false);
});

test('empty, missing and malformed allowlists do not imply unrestricted support', () => {
    for (const reasoning of [{}, { supported_efforts: [] }, { supported_efforts: 'high' },
        { supported_efforts: ['unknown'] }, { mandatory: false }]) {
        assert.equal(parseModelReasoning({ reasoning }).status, 'unknown');
    }
});

test('budget support must be a boolean declaration and has no invented bounds', () => {
    assert.equal(parseModelReasoning({ reasoning: { supports_max_tokens: 'true' } }).status, 'unknown');
    const result = parseModelReasoning({ reasoning: { supports_max_tokens: true } });
    assert.equal(result.status, 'supported');
    assert.equal(result.supportsBudget, true);
    assert.deepEqual(result.efforts, []);
    assert.equal(result.minBudget, undefined);
});

test('none is explicit, unknown default effort is not silently adopted', () => {
    const result = parseModelReasoning({ reasoning: { supported_efforts: ['none', 'low'], default_effort: 'max' } });
    assert.equal(result.canDisable, true);
    assert.equal(result.defaultEffort, null);
});

test('VCP capability is derived, other upstream metadata remains intact', () => {
    const model = { id: 'alias', pricing: { prompt: '0' }, vcpReasoning: { status: 'supported', efforts: ['high'] } };
    const result = attachModelReasoning(model);
    assert.equal(result.vcpReasoning.status, 'unknown');
    assert.equal(result.id, model.id);
    assert.deepEqual(result.pricing, model.pricing);
    assert.equal(model.vcpReasoning.status, 'supported');
});
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { ModelReasoningService } = require('../modules/modelReasoningService');
const selection = value => ({ version: 1, mode: 'effort', value });
function fixture(models) {
    const service = new ModelReasoningService();
    service.overrides = async () => ({});
    if (models !== undefined) service.seedCatalog({ data: models });
    return { service };
}
test('default is transparent and does not resolve metadata', async () => {
    const { service } = fixture([]);
    service.resolve = async () => { throw new Error('must not resolve'); };
    const body = { model: 'a', thinking: { type: 'enabled' } };
    assert.equal(await service.adapt(body, { mode: 'default' }), body);
    assert.equal(await service.adapt(body), body);
});
test('100 queries and adaptations make zero upstream requests; seed activates declarations', async t => {
    let calls = 0;
    const forbidden = () => { calls++; throw new Error('unexpected upstream request'); };
    t.mock.method(globalThis, 'fetch', forbidden);
    t.mock.method(require('node:http'), 'request', forbidden);
    t.mock.method(require('node:https'), 'request', forbidden);
    const { service } = fixture();
    const results = await Promise.all(Array.from({ length: 100 }, () => service.resolve('a')));
    assert.ok(results.every(result => result.manual && result.status === 'unknown'));
    await Promise.all(Array.from({ length: 100 }, () => service.adapt({ model: 'a' }, selection('high'))));
    service.seedCatalog({ data: [{ id: 'a', reasoning: { supported_efforts: ['high'] } }] });
    assert.equal((await service.resolve('a')).status, 'supported');
    assert.deepEqual((await service.adapt({ model: 'a' }, selection('high'))).reasoning, { effort: 'high' });
    assert.equal(calls, 0);
});
test('explicit selection replaces conflicting controls without changing loop body', async () => {
    const { service } = fixture([{ id: 'a', reasoning: { supported_efforts: ['high'] } }]);
    const body = { model: 'a', thinking: { type: 'enabled' }, reasoning_effort: 'low', messages: [] };
    const next = await service.adapt(body, selection('high'));
    assert.deepEqual(next.reasoning, { effort: 'high' });
    assert.equal(next.thinking, undefined);
    assert.equal(next.reasoning_effort, undefined);
    assert.equal(body.reasoning_effort, 'low');
});
test('fallback target is validated independently', async () => {
    const { service } = fixture([
        { id: 'a', reasoning: { supported_efforts: ['high'] } },
        { id: 'b', reasoning: { supported_efforts: ['low'] } }
    ]);
    await service.adapt({ model: 'a' }, selection('high'));
    await assert.rejects(service.adapt({ model: 'b' }, selection('high')), { code: 'INVALID_REASONING_CONFIG' });
});
test('missing catalog offers manual efforts but no budget; invalid seed is ignored', async () => {
    const { service } = fixture();
    service.seedCatalog(null);
    service.seedCatalog({ data: null });
    const capability = await service.resolve('a');
    assert.equal(capability.status, 'unknown');
    assert.equal(capability.manual, true);
    assert.equal(capability.adapter, 'reasoning_effort');
    assert.equal((await service.adapt({ model: 'a' }, selection('high'))).reasoning_effort, 'high');
    await assert.rejects(service.adapt({ model: 'a' }, { version: 1, mode: 'budget', value: 100 }),
        { code: 'INVALID_REASONING_CONFIG' });
});
test('budget validates integer, positivity and output limit', async () => {
    const { service } = fixture([{ id: 'a', reasoning: { supports_max_tokens: true } }]);
    for (const value of [0, -1, 0.5, '100', NaN, Infinity, 1000]) {
        await assert.rejects(service.adapt({ model: 'a', max_tokens: 1000 }, { version: 1, mode: 'budget', value }),
            { code: 'INVALID_REASONING_CONFIG' });
    }
    const result = await service.adapt({ model: 'a', max_tokens: 1000 }, { version: 1, mode: 'budget', value: 100 });
    assert.deepEqual(result.reasoning, { max_tokens: 100 });
});
test('exact overrides choose wire protocol without model name guessing', async () => {
    const { service } = fixture([]);
    service.overrides = async () => ({
        alias: { adapter: 'reasoning_effort', supported_efforts: ['high'] },
        budget: { adapter: 'thinking', supports_max_tokens: true }
    });
    assert.equal((await service.adapt({ model: 'alias' }, selection('high'))).reasoning_effort, 'high');
    assert.deepEqual((await service.adapt({ model: 'budget' }, { version: 1, mode: 'budget', value: 200 })).thinking,
        { type: 'enabled', budget_tokens: 200 });
});
test('unrecognized mode and version are rejected', async () => {
    const { service } = fixture([]);
    for (const config of [{ version: 2, mode: 'effort', value: 'high' }, { version: 1, mode: 'alien' }]) {
        await assert.rejects(service.adapt({ model: 'a' }, config), { code: 'INVALID_REASONING_CONFIG' });
    }
});

test('missing metadata allows all manual efforts without claiming support or probing chat', async () => {
    const { EFFORTS } = require('../modules/modelReasoningCapabilities');
    const { service } = fixture([{ id: 'a' }]);
    for (const model of ['a', 'absent']) {
        const capability = await service.resolve(model);
        assert.equal(capability.status, 'unknown');
        assert.equal(capability.source, 'manual');
        assert.equal(capability.manual, true);
        assert.deepEqual(capability.efforts, EFFORTS);
        for (const effort of EFFORTS) {
            assert.equal((await service.adapt({ model }, selection(effort))).reasoning_effort, effort);
        }
        await assert.rejects(service.adapt({ model }, selection('invented')));
    }
});

test('explicit empty, invalid, mandatory metadata and overrides stay strict', async () => {
    for (const reasoning of [{}, null, false, { supported_efforts: [] },
        { mandatory: true, supported_efforts: ['high', 'none'] }]) {
        const { service } = fixture([{ id: 'a', reasoning }]);
        assert.notEqual((await service.resolve('a')).manual, true);
        await assert.rejects(service.adapt({ model: 'a' }, selection('none')));
    }
    const { service } = fixture([]);
    service.overrides = async () => ({ a: { adapter: 'reasoning_effort', supported_efforts: ['low'] } });
    await assert.rejects(service.adapt({ model: 'a' }, selection('high')));
});

// Execute only the registered route to avoid starting the full plugin/server runtime.
async function modelsRoute(service, fetch, virtualModels = []) {
    const fs = require('node:fs');
    const vm = require('node:vm');
    const source = fs.readFileSync(require.resolve('../server'), 'utf8');
    const start = source.indexOf("app.get('/v1/models',");
    const end = source.indexOf('// 新增：标准化任务创建API端点', start);
    let handler;
    vm.runInNewContext(source.slice(start, end).replace("await import('node-fetch')", '({ default: mockFetch })'), {
        app: { get: (route, callback) => { handler = callback; } }, mockFetch: fetch,
        apiUrl: 'https://fixture.invalid', apiKey: 'fixture',
        require: () => ({ modelReasoningService: service }),
        semanticModelRouter: { getVirtualModels: () => virtualModels },
        modelRedirectHandler: { isEnabled: () => true, redirectModelForClient: id => `public-${id}` },
        DEBUG_MODE: false, console: { error() {}, warn() {} }, Date
    });
    const res = {
        headers: {}, statusCode: 200,
        status(code) { this.statusCode = code; return this; },
        setHeader(name, value) { this.headers[name] = value; },
        type(value) { this.contentType = value; return this; },
        json(body) { this.body = body; return this; },
        send(body) { this.body = body; return this; }
    };
    await handler({ headers: {} }, res);
    return res;
}

test('models route keeps per-request fetch and seeds raw ids before aliases and virtual models', async () => {
    const { service } = fixture();
    let calls = 0;
    const fetch = async (url, options) => {
        calls++;
        assert.equal(url, 'https://fixture.invalid/v1/models');
        assert.equal(options.method, 'GET');
        assert.equal(options.headers.Authorization, 'Bearer fixture');
        assert.equal(options.headers.Accept, 'application/json');
        return { ok: true, status: 201, headers: new Map([['x-upstream', 'yes'], ['content-length', '10']]),
            text: async () => JSON.stringify({ data: [{ id: 'a', reasoning: { supported_efforts: ['high'] } }] }) };
    };
    for (let i = 0; i < 2; i++) {
        const res = await modelsRoute(service, fetch, [{ id: 'virtual' }]);
        assert.equal(res.statusCode, 201);
        assert.deepEqual(Array.from(res.body.data, model => model.id), ['public-a', 'virtual']);
        assert.equal(res.headers['x-upstream'], 'yes');
        assert.equal(res.headers['content-length'], undefined);
    }
    assert.equal(calls, 2);
    assert.equal((await service.resolve('a')).status, 'supported');
    assert.equal((await service.resolve('public-a')).manual, true);
    assert.equal((await service.resolve('virtual')).manual, true);
    assert.equal(calls, 2);
});

test('models route preserves original failures, parse errors and virtual fallback', async () => {
    const service = { seedCatalog() { throw new Error('must not seed failures'); } };
    const fetch = async () => ({ ok: false, status: 429, text: async () => 'limited' });
    const failed = await modelsRoute(service, fetch);
    assert.equal(failed.statusCode, 429);
    assert.equal(failed.body, 'limited');
    assert.equal(failed.contentType, 'text/plain');
    const fallback = await modelsRoute(service, fetch, [{ id: 'virtual' }]);
    assert.equal(fallback.statusCode, 200);
    assert.equal(fallback.body.data[0].id, 'virtual');
    const invalidFetch = async () => {
        let consumed = false;
        return { ok: true, status: 200, text: async () => {
            if (consumed) throw new Error('body used already');
            consumed = true;
            return 'not-json';
        } };
    };
    assert.equal((await modelsRoute(service, invalidFetch)).statusCode, 500);
    assert.equal((await modelsRoute(service, invalidFetch, [{ id: 'virtual' }])).statusCode, 200);
    for (const virtual of [[], [{ id: 'virtual' }]]) {
        const offline = await modelsRoute(service, async () => { throw new Error('offline'); }, virtual);
        assert.equal(offline.statusCode, 500);
        assert.equal(offline.body.error, 'Internal Server Error');
    }
});
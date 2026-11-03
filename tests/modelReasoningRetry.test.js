'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const http = require('node:http');
const Handler = require('../modules/chatCompletionHandler');
const { ModelReasoningService } = require('../modules/modelReasoningService');

test('real retry HTTP bodies adapt after fallback selection and continuation repeats the rule', async t => {
    const bodies = [];
    const server = http.createServer((req, res) => {
        let text = '';
        req.setEncoding('utf8');
        req.on('data', part => { text += part; });
        req.on('end', () => {
            bodies.push(JSON.parse(text));
            res.writeHead(bodies.length === 1 ? 503 : 200, { 'Content-Type': 'application/json' });
            res.end('{"ok":true}');
        });
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
    const service = new ModelReasoningService();
    service.overrides = async () => ({
        a: { adapter: 'reasoning', supported_efforts: ['high'] },
        b: { adapter: 'reasoning_effort', supported_efforts: ['high'] },
        restricted: { adapter: 'reasoning_effort', supported_efforts: ['low'] }
    });
    const prepareAttempt = async options => ({
        ...options,
        body: JSON.stringify(await service.adapt(JSON.parse(options.body), { version: 1, mode: 'effort', value: 'high' }))
    });
    const url = `http://127.0.0.1:${server.address().port}/v1/chat/completions`;
    const response = await Handler.fetchWithRetry(url, {
        method: 'POST', body: JSON.stringify({ model: 'virtual', messages: [], thinking: { type: 'enabled' } })
    }, { retries: 2, delay: 1, modelFallbackCandidates: ['a', 'b'], prepareAttempt });
    assert.equal(response.status, 200);
    await response.text();
    assert.deepEqual(bodies[0].reasoning, { effort: 'high' });
    assert.equal(bodies[1].model, 'b');
    assert.equal(bodies[1].reasoning_effort, 'high');
    assert.equal(bodies[1].reasoning, undefined);
    assert.equal(bodies[0].thinking, undefined);
    const continuation = await Handler.fetchWithRetry(url, {
        method: 'POST', body: JSON.stringify({ model: 'b', messages: [{ role: 'user', content: 'tool result' }] })
    }, { retries: 1, prepareAttempt });
    await continuation.text();
    assert.equal(bodies[2].reasoning_effort, 'high');
    assert.equal(bodies[2].messages[0].content, 'tool result');

    await assert.rejects(Handler.fetchWithRetry(url, {
        method: 'POST', body: JSON.stringify({ model: 'restricted' })
    }, { retries: 3, delay: 1, prepareAttempt }), { code: 'INVALID_REASONING_CONFIG' });
    assert.equal(bodies.length, 3, 'invalid selection never reaches HTTP');
});

test('abort during asynchronous capability preparation never starts an HTTP attempt', async () => {
    const controller = new AbortController();
    await assert.rejects(Handler.fetchWithRetry('http://127.0.0.1:1/must-not-connect', {
        method: 'POST', body: '{}', signal: controller.signal
    }, {
        retries: 1,
        prepareAttempt: async options => { controller.abort(); return options; }
    }), { name: 'AbortError' });
});
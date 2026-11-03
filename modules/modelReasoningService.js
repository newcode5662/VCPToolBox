'use strict';

const fs = require('node:fs/promises');
const path = require('node:path');
const { EFFORTS, parseModelReasoning } = require('./modelReasoningCapabilities');

class ReasoningConfigError extends Error {
    constructor(message) {
        super(message);
        this.name = 'ReasoningConfigError';
        this.code = 'INVALID_REASONING_CONFIG';
        this.status = 400;
    }
}

class ModelReasoningService {
    constructor({ overridesPath } = {}) {
        this.overridesPath = overridesPath;
        this.models = new Map();
    }

    seedCatalog(body) {
        if (!body || !Array.isArray(body.data)) return;
        const models = new Map();
        for (const model of body.data.slice(0, 10000)) {
            if (model && typeof model.id === 'string') models.set(model.id, model);
        }
        this.models = models;
    }

    async overrides() {
        const file = this.overridesPath || path.join(__dirname, '..', 'model-reasoning.json');
        try {
            const text = await fs.readFile(file, 'utf8');
            if (Buffer.byteLength(text) > 1024 * 1024) throw new Error('Reasoning overrides exceed 1 MiB');
            const config = JSON.parse(text);
            if (!config || config.version !== 1 || !config.models || Array.isArray(config.models)) {
                throw new Error('Invalid model-reasoning.json schema');
            }
            return config.models;
        } catch (error) {
            if (error.code === 'ENOENT') return {};
            throw error;
        }
    }

    async resolve(modelId) {
        const overrides = await this.overrides();
        if (Object.prototype.hasOwnProperty.call(overrides, modelId)) {
            const rule = overrides[modelId];
            if (!rule || typeof rule !== 'object') throw new Error('Invalid reasoning override');
            const capability = parseModelReasoning({ reasoning: rule });
            const adapter = rule.adapter || null;
            if (!['reasoning', 'reasoning_effort', 'thinking_budget', 'thinking'].includes(adapter)) {
                throw new Error('Invalid reasoning override adapter');
            }
            const efforts = ['reasoning', 'reasoning_effort'].includes(adapter) ? capability.efforts : [];
            const supportsBudget = adapter !== 'reasoning_effort' && capability.supportsBudget;
            return { ...capability, source: 'override', adapter, efforts, supportsBudget,
                status: efforts.length || supportsBudget ? 'supported' : 'unknown' };
        }
        const model = this.models.get(modelId);
        const capability = parseModelReasoning(model);
        // Manual passthrough is not a claim of verified support. Explicit declarations
        // (including empty/invalid ones) must never be widened to manual controls.
        if (!model || !Object.prototype.hasOwnProperty.call(model, 'reasoning')) {
            return { ...capability, source: 'manual', status: 'unknown', efforts: [...EFFORTS],
                adapter: 'reasoning_effort', manual: true };
        }
        return { ...capability, adapter: capability.status === 'supported' ? 'reasoning' : null };
    }

    async adapt(body, selection) {
        if (!selection || selection.mode === 'default') return body;
        if (selection.version !== 1 || !['effort', 'budget'].includes(selection.mode)) {
            throw new ReasoningConfigError('无效的思考配置版本或模式');
        }
        const capability = await this.resolve(body.model);
        if (capability.status !== 'supported' && !(capability.manual === true && selection.mode === 'effort')) {
            throw new ReasoningConfigError(`模型 ${body.model} 未声明可用思考配置`);
        }
        if (!['reasoning', 'reasoning_effort', 'thinking_budget', 'thinking'].includes(capability.adapter)) {
            throw new ReasoningConfigError(`模型 ${body.model} 未配置思考参数协议`);
        }
        const result = { ...body };
        // Explicit selection owns these mutually exclusive controls; never mutate the loop body.
        delete result.reasoning;
        delete result.reasoning_effort;
        delete result.thinking;
        delete result.thinking_config;
        if (selection.mode === 'effort') {
            if (!capability.efforts.includes(selection.value)) {
                throw new ReasoningConfigError(`模型 ${body.model} 不支持思考强度 ${selection.value}`);
            }
            if (capability.adapter === 'reasoning') {
                result.reasoning = { effort: selection.value };
            } else if (capability.adapter === 'reasoning_effort') {
                result.reasoning_effort = selection.value;
            } else {
                throw new ReasoningConfigError('当前协议仅支持思考预算，不能发送强度档位');
            }
        } else {
            if (!capability.supportsBudget || !Number.isSafeInteger(selection.value) || selection.value <= 0) {
                throw new ReasoningConfigError('思考预算必须为模型支持的正整数');
            }
            if (Number.isFinite(body.max_tokens) && selection.value >= body.max_tokens) {
                throw new ReasoningConfigError('思考预算必须小于最大输出 token');
            }
            if (capability.adapter === 'reasoning') result.reasoning = { max_tokens: selection.value };
            else if (capability.adapter === 'thinking') result.thinking = { type: 'enabled', budget_tokens: selection.value };
            else if (capability.adapter === 'thinking_budget') result.thinking_config = { thinking_budget: selection.value };
            else throw new ReasoningConfigError('当前协议不支持思考预算');
        }
        return result;
    }
}

const modelReasoningService = new ModelReasoningService();
module.exports = { ModelReasoningService, ReasoningConfigError, modelReasoningService };
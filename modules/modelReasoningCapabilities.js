'use strict';

const EFFORTS = Object.freeze(['max', 'xhigh', 'high', 'medium', 'low', 'minimal', 'none']);
const has = (object, key) => Object.prototype.hasOwnProperty.call(object, key);
const unknown = () => ({
    version: 1,
    status: 'unknown',
    source: 'none',
    efforts: [],
    supportsBudget: false,
    canDisable: null,
    defaultEffort: null,
    defaultEnabled: null
});

/**
 * Parse explicit catalog declarations only. A missing reasoning object is ambiguous
 * (including dynamic routers), not evidence of non-support. Does not infer wire format.
 */
function parseModelReasoning(model) {
    if (!model || typeof model !== 'object' || Array.isArray(model)) return unknown();
    const reasoning = model.reasoning;
    if (!reasoning || typeof reasoning !== 'object' || Array.isArray(reasoning)) return unknown();

    const mandatory = reasoning.mandatory;
    const declaresEfforts = has(reasoning, 'supported_efforts');
    let efforts = [];
    if (declaresEfforts) {
        if (reasoning.supported_efforts === null) {
            efforts = [...EFFORTS];
        } else if (Array.isArray(reasoning.supported_efforts)) {
            efforts = [...new Set(reasoning.supported_efforts.filter(value => EFFORTS.includes(value)))];
        }
    }
    if (mandatory === true) efforts = efforts.filter(value => value !== 'none');
    const supportsBudget = reasoning.supports_max_tokens === true;
    // Invalid or partial metadata must not manufacture a usable control.
    const status = efforts.length || supportsBudget ? 'supported' : 'unknown';
    return {
        version: 1,
        status,
        source: 'upstream',
        efforts,
        supportsBudget,
        canDisable: mandatory === true ? false : efforts.includes('none') ? true : null,
        defaultEffort: efforts.includes(reasoning.default_effort) ? reasoning.default_effort : null,
        defaultEnabled: typeof reasoning.default_enabled === 'boolean' ? reasoning.default_enabled : null
    };
}

function attachModelReasoning(model) {
    if (!model || typeof model !== 'object' || Array.isArray(model)) return model;
    // Reserved VCP field is always derived locally, never trusted from upstream.
    return { ...model, vcpReasoning: parseModelReasoning(model) };
}

module.exports = { EFFORTS, parseModelReasoning, attachModelReasoning };
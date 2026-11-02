'use strict';
const crypto = require('node:crypto');
const COMMANDS = new Set(['WriteFile', 'AppendFile', 'EditFile']);
const digest = bytes => crypto.createHash('sha256').update(bytes).digest('hex');

function normalizeRequest(input) {
    if (!input || !COMMANDS.has(input.command)) throw new Error('WRITE_COMMAND_UNSUPPORTED');
    if (typeof input.filePath !== 'string' || !input.filePath || input.filePath.includes('\0')) {
        throw new Error('WRITE_PATH_REQUIRED');
    }
    if (typeof input.content !== 'string') throw new Error('WRITE_CONTENT_REQUIRED');
    const encoding = input.encoding === undefined ? 'utf8' : input.encoding;
    if (typeof encoding !== 'string' || !Buffer.isEncoding(encoding)) throw new Error('WRITE_ENCODING_INVALID');
    const operationId = input.operationId ?? null;
    if (operationId !== null && (typeof operationId !== 'string'
        || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(operationId))) {
        throw new Error('OPERATION_ID_INVALID');
    }
    const expectedRevision = input.expectedRevision ?? null;
    if (expectedRevision !== null && expectedRevision !== 'missing'
        && (typeof expectedRevision !== 'string' || !/^[a-f0-9]{64}$/.test(expectedRevision))) {
        throw new Error('EXPECTED_REVISION_INVALID');
    }
    const conflictPolicy = input.conflictPolicy ?? 'rename';
    if (!['rename', 'error', 'overwrite'].includes(conflictPolicy)) throw new Error('CONFLICT_POLICY_INVALID');
    if (input.command !== 'WriteFile' && input.conflictPolicy !== undefined) {
        throw new Error('CONFLICT_POLICY_NOT_APPLICABLE');
    }
    return { command: input.command, filePath: input.filePath,
        content: input.content, encoding, operationId, expectedRevision, conflictPolicy };
}

/**
 * Compute operation identity BEFORE reading the current target or enforcing
 * expectedRevision. A committed retry must replay its historical receipt even
 * when the target has subsequently changed. Caller still authorizes the path.
 */
function identifyWrite(input, { preserveAppendEol = false } = {}) {
    if (typeof preserveAppendEol !== 'boolean') throw new Error('WRITE_EOL_POLICY_INVALID');
    const request = normalizeRequest(input);
    const requestFingerprint = digest(Buffer.from(JSON.stringify({
        contractVersion: 1, ...request, operationId: undefined, preserveAppendEol,
    })));
    return { request, operationId: request.operationId, requestFingerprint };
}
function preserveLineEndings(original, content) {
    const crlf = (original.match(/\r\n/g) || []).length;
    const lf = (original.match(/[^\r]\n/g) || []).length + Number(original.startsWith('\n'));
    const cr = (original.match(/\r(?!\n)/g) || []).length;
    const style = crlf > lf && crlf > cr ? '\r\n' : cr > lf && cr > crlf ? '\r' : '\n';
    return content.replace(/\r\n/g, '\n').replace(/\r/g, '\n').replace(/\n/g, style);
}

/**
 * Caller supplies an authorized, normalized path and a bounded exact byte
 * snapshot. Never use this planner as a path authorization or file lock.
 * rename_required is a decision, NOT a reserved/unique output path.
 */
function planWrite(input, existing, { maxBytes = 20 * 1024 * 1024, preserveAppendEol = false } = {}) {
    const identity = identifyWrite(input, { preserveAppendEol });
    const { request, requestFingerprint } = identity;
    if (existing !== null && !Buffer.isBuffer(existing)) throw new Error('WRITE_SNAPSHOT_INVALID');
    if (!Number.isSafeInteger(maxBytes) || maxBytes < 0) throw new Error('WRITE_LIMIT_INVALID');
    if (existing && existing.length > maxBytes) throw new Error('WRITE_SNAPSHOT_TOO_LARGE');
    const beforeRevision = existing === null ? 'missing' : digest(existing);
    if (request.expectedRevision !== null && request.expectedRevision !== beforeRevision) {
        throw new Error('REVISION_CONFLICT');
    }
    if (request.command === 'EditFile' && existing === null) throw new Error('EDIT_REQUIRES_EXISTING_FILE');
    let disposition = 'replace';
    if (request.command === 'WriteFile' && existing !== null) {
        if (request.conflictPolicy === 'error') throw new Error('FILE_ALREADY_EXISTS');
        if (request.conflictPolicy === 'rename') disposition = 'rename_required';
    }
    let content = request.content;
    if (existing !== null && (request.command === 'EditFile'
        || (request.command === 'AppendFile' && preserveAppendEol))) {
        content = preserveLineEndings(existing.toString(request.encoding), content);
    }
    const addition = Buffer.from(content, request.encoding);
    const size = request.command === 'AppendFile' ? (existing?.length || 0) + addition.length : addition.length;
    if (size > maxBytes) throw new Error('WRITE_SIZE_LIMIT');
    const bytes = request.command === 'AppendFile'
        ? Buffer.concat([existing || Buffer.alloc(0), addition]) : addition;
    // Binding the raw requested content prevents the same ID from silently
    // accepting a different payload that happens to render to identical bytes.
    return { contractVersion: 1, operationId: request.operationId,
        requestFingerprint, beforeRevision, afterRevision: digest(bytes),
        bytes, size, disposition };
}

function classifyOperation(previous, plan) {
    if (!previous) return 'new';
    if (previous.requestFingerprint !== plan.requestFingerprint) throw new Error('OPERATION_ID_PAYLOAD_CONFLICT');
    if (previous.state === 'retired') throw new Error('OPERATION_RETIRED');
    if (previous.state === 'committed') return 'replay';
    if (previous.state === 'prepared') return 'reconcile_required';
    throw new Error('OPERATION_STATE_INVALID');
}

module.exports = { identifyWrite, planWrite, classifyOperation };

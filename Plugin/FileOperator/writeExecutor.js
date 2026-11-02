'use strict';
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { identifyWrite, planWrite, classifyOperation } = require('./writePlan');
const { createLedger } = require('./operationLedger');
const hash = bytes => crypto.createHash('sha256').update(bytes).digest('hex');

/**
 * Experimental cooperative executor, NOT connected to FileOperator dispatch.
 * authorize(canonicalPath, command) must explicitly return true.
 * One gate per ledger root covers operation lookup through target publication
 * and receipt commit. Other roots/legacy writers/external editors are NOT locked.
 * onPhase is trusted synchronous fault-injection instrumentation, not user input.
 */
function createWriteExecutor({ ledgerRoot, authorize, maxBytes = 20 * 1024 * 1024,
    preserveAppendEol = false, onPhase = () => {} }) {
    if (typeof authorize !== 'function') throw new Error('WRITE_AUTHORIZER_REQUIRED');
    if (!Number.isSafeInteger(maxBytes) || maxBytes < 0) throw new Error('WRITE_LIMIT_INVALID');
    const ledger = createLedger(ledgerRoot);
    const gatePath = path.join(ledgerRoot, 'executor.lock');

    function authorizedTarget(raw, command) {
        if (typeof raw !== 'string' || !path.isAbsolute(raw)) throw new Error('WRITE_ABSOLUTE_PATH_REQUIRED');
        const resolved = path.resolve(raw);
        const parent = fs.realpathSync(path.dirname(resolved));
        const target = path.join(parent, path.basename(resolved));
        if (authorize(target, command) !== true) throw new Error('WRITE_ACCESS_DENIED');
        return target;
    }
    function readTarget(target) {
        let meta;
        try { meta = fs.lstatSync(target); }
        catch (error) { if (error.code === 'ENOENT') return null; throw error; }
        if (!meta.isFile() || meta.isSymbolicLink()) throw new Error('WRITE_TARGET_INVALID');
        const fd = fs.openSync(target, 'r');
        const chunks = [];
        let size = 0;
        try {
            const buffer = Buffer.alloc(65536);
            while (true) {
                const n = fs.readSync(fd, buffer, 0, Math.min(buffer.length, maxBytes - size + 1), null);
                if (!n) break;
                size += n;
                if (size > maxBytes) throw new Error('WRITE_SNAPSHOT_TOO_LARGE');
                chunks.push(Buffer.from(buffer.subarray(0, n)));
            }
        } finally { fs.closeSync(fd); }
        return Buffer.concat(chunks);
    }
    function revision(bytes) { return bytes === null ? 'missing' : hash(bytes); }
    function withGate(action) {
        const fd = fs.openSync(gatePath, 'wx', 0o600);
        const cleanupWarnings = [];
        let result, failure;
        try { result = action(cleanupWarnings); }
        catch (error) { failure = error; }
        finally {
            try { fs.closeSync(fd); } catch (error) { cleanupWarnings.push(error.message); }
            try { fs.unlinkSync(gatePath); } catch (error) { cleanupWarnings.push(error.message); }
        }
        if (failure) { failure.cleanupWarnings = cleanupWarnings; throw failure; }
        return { ...result, cleanupWarnings };
    }
    function execute(input) {
        const target = authorizedTarget(input.filePath, input.command);
        const identity = identifyWrite({ ...input, filePath: target }, { preserveAppendEol });
        if (!identity.operationId) throw new Error('EXECUTOR_OPERATION_ID_REQUIRED');

        return withGate(warnings => {
            const previous = ledger.query(identity.operationId);
            const classification = classifyOperation(previous.state === 'missing' ? null : previous, identity);
            if (classification === 'replay') {
                // Historical receipt, NOT a statement about the current target.
                authorizedTarget(previous.receipt.path, input.command);
                return { state: 'committed', replayed: true, receipt: previous.receipt };
            }
            if (classification === 'reconcile_required') {
                return { state: 'prepared', outcomeUncertain: true, requiresReconciliation: true };
            }
            const original = readTarget(target);
            const plan = planWrite({ ...input, filePath: target }, original, { maxBytes, preserveAppendEol });
            let output = target;
            if (plan.disposition === 'rename_required') {
                const extension = path.extname(target);
                const base = path.basename(target, extension);
                let selected = false;
                for (let n = 1; n <= 10000; n++) {
                    const candidate = path.join(path.dirname(target), `${base}(${n})${extension}`);
                    try { fs.lstatSync(candidate); }
                    catch (error) {
                        if (error.code !== 'ENOENT') throw error;
                        output = authorizedTarget(candidate, input.command);
                        selected = true;
                        break;
                    }
                }
                if (!selected) throw new Error('WRITE_RENAME_LIMIT');
            }
            const intent = { command: input.command, path: output, originalPath: target,
                beforeRevision: plan.beforeRevision, afterRevision: plan.afterRevision, size: plan.size };
            const prepared = ledger.prepare(identity.operationId, identity.requestFingerprint, intent);
            warnings.push(...prepared.cleanupWarnings);
            let temp = null;
            let targetPublished = false;
            try {
                onPhase('prepared');
                const candidate = path.join(path.dirname(target), `.vcp-write-${crypto.randomUUID()}.tmp`);
                if (authorize(candidate, input.command) !== true) throw new Error('WRITE_TEMP_ACCESS_DENIED');
                const fd = fs.openSync(candidate, 'wx', 0o600);
                temp = candidate;
                try {
                    // Preserve existing permissions for replacement; creation remains private.
                    if (original !== null && output === target) {
                        fs.fchmodSync(fd, fs.statSync(target).mode & 0o777);
                    }
                    fs.writeFileSync(fd, plan.bytes);
                    fs.fsyncSync(fd);
                } finally { fs.closeSync(fd); }
                onPhase('staged');
                if (authorizedTarget(target, input.command) !== target) throw new Error('WRITE_PARENT_CHANGED');
                if (revision(readTarget(target)) !== plan.beforeRevision) throw new Error('REVISION_CONFLICT');
                // No-clobber publication for missing targets; replacement is atomic
                // rename, but the preceding revision read is not an OS-level CAS.
                if (authorizedTarget(output, input.command) !== output) throw new Error('WRITE_PARENT_CHANGED');
                if (original === null || output !== target) fs.linkSync(temp, output);
                else fs.renameSync(temp, output);
                targetPublished = true;
                onPhase('target_published');
                const receipt = { operationId: identity.operationId, path: output,
                    originalPath: target, renamed: output !== target,
                    beforeRevision: plan.beforeRevision, revision: plan.afterRevision, size: plan.size };
                const committed = ledger.commit(identity.operationId, identity.requestFingerprint, receipt);
                warnings.push(...committed.cleanupWarnings);
                return { state: 'committed', replayed: false, receipt: committed.record.receipt };
            } catch (error) {
                return { state: 'prepared', outcomeUncertain: true, requiresReconciliation: true,
                    targetPublicationConfirmed: targetPublished, error: error.message };
            } finally {
                if (temp) {
                    try { fs.unlinkSync(temp); }
                    catch (error) { if (error.code !== 'ENOENT') warnings.push(error.message); }
                }
            }
        });
    }
    function query(operationId) {
        // Authorize the stored target before disclosing intent or historical receipt.
        return withGate(() => {
            const record = ledger.query(operationId);
            if (record.state === 'missing') return { record };
            // committed records intentionally retain receipt, intent lives in prepared.
            const target = record.state === 'retired' ? record.path
                : record.state === 'committed' ? record.receipt?.path : record.intent?.path;
            if (typeof target !== 'string') throw new Error('LEDGER_TARGET_INVALID');
            authorizedTarget(target, 'QueryOperation');
            return { record };
        });
    }
    function retire(operationId, expectedFingerprint) {
        return withGate(warnings => {
            const record = ledger.query(operationId);
            if (record.state === 'missing') throw new Error('OPERATION_NOT_FOUND');
            const target = record.state === 'retired' ? record.path
                : record.state === 'committed' ? record.receipt?.path : record.intent?.path;
            if (typeof target !== 'string') throw new Error('LEDGER_TARGET_INVALID');
            authorizedTarget(target, 'CleanupOperation');
            const result = ledger.retire(operationId, expectedFingerprint);
            warnings.push(...result.cleanupWarnings);
            return { record: result.record };
        });
    }
    return { execute, query, retire };
}
module.exports = { createWriteExecutor };

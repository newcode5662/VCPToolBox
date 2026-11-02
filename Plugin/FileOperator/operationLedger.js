'use strict';
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const LIMIT = 256 * 1024;
const hash = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const validHash = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
function requireId(id) {
    if (typeof id !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(id)) {
        throw new Error('OPERATION_ID_INVALID');
    }
}
function readRecord(file, io = fs) {
    const fs = io;
    let meta;
    try { meta = fs.lstatSync(file); }
    catch (error) { if (error.code === 'ENOENT') return null; throw error; }
    if (!meta.isFile() || meta.isSymbolicLink() || meta.size > LIMIT) throw new Error('LEDGER_RECORD_INVALID');
    const fd = fs.openSync(file, 'r');
    const bytes = Buffer.alloc(LIMIT + 1);
    let length = 0;
    try {
        while (length < bytes.length) {
            const n = fs.readSync(fd, bytes, length, bytes.length - length, null);
            if (!n) break;
            length += n;
        }
    } finally { fs.closeSync(fd); }
    if (length > LIMIT) throw new Error('LEDGER_RECORD_TOO_LARGE');
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length)));
}
function publish(file, value, io = fs) {
    const bytes = Buffer.from(JSON.stringify(value));
    if (bytes.length > LIMIT) throw new Error('LEDGER_RECORD_TOO_LARGE');
    const temp = `${file}.tmp.${crypto.randomUUID()}`;
    // Only acquire cleanup ownership after exclusive creation succeeds.
    const fd = io.openSync(temp, 'wx', 0o600);
    let failure = null;
    let closed = false;
    let published = false;
    const cleanupWarnings = [];
    try {
        io.writeFileSync(fd, bytes);
        io.fsyncSync(fd);
        io.closeSync(fd);
        closed = true;
        // Complete immutable record; never overwrite a previous publication.
        io.linkSync(temp, file);
        published = true;
    } catch (error) {
        failure = error;
    } finally {
        if (!closed) {
            try { io.closeSync(fd); }
            catch (error) { cleanupWarnings.push(error.message); }
        }
        try { io.unlinkSync(temp); }
        catch (error) { cleanupWarnings.push(error.message); }
    }
    if (failure) {
        failure.cleanupWarnings = cleanupWarnings;
        failure.recordPublished = published;
        throw failure;
    }
    return { published, cleanupWarnings };
}
function createLedger(root, { fileSystem = fs } = {}) {
    const io = fileSystem;
    if (typeof root !== 'string' || !path.isAbsolute(root)) throw new Error('LEDGER_ROOT_REQUIRED');
    io.mkdirSync(root, { recursive: true });
    const meta = io.lstatSync(root);
    if (!meta.isDirectory() || meta.isSymbolicLink()) throw new Error('LEDGER_ROOT_INVALID');
    const paths = id => {
        requireId(id);
        const stem = path.join(root, hash(Buffer.from(id)));
        return { prepared: `${stem}.prepared.json`, committed: `${stem}.committed.json`,
            retired: `${stem}.retired.json`, lock: `${stem}.lock` };
    };
    function query(id) {
        const files = paths(id);
        // Retire marker is published BEFORE details are removed. It remains the
        // authoritative gate even if cleanup was interrupted halfway.
        const retired = readRecord(files.retired, io);
        if (retired) {
            if (retired.schemaVersion !== 1 || retired.state !== 'retired'
                || retired.operationId !== id || !validHash(retired.requestFingerprint)
                || !validHash(retired.committedRevision)
                || typeof retired.path !== 'string' || !path.isAbsolute(retired.path)) {
                throw new Error('LEDGER_STATE_CORRUPT');
            }
            return retired;
        }
        const prepared = readRecord(files.prepared, io);
        const committed = readRecord(files.committed, io);
        if (!prepared && committed) throw new Error('LEDGER_STATE_CORRUPT');
        if (!prepared) return { state: 'missing', operationId: id };
        if (prepared.schemaVersion !== 1 || prepared.state !== 'prepared'
            || prepared.operationId !== id || !validHash(prepared.requestFingerprint)) {
            throw new Error('LEDGER_STATE_CORRUPT');
        }
        if (!committed) return prepared;
        if (committed.schemaVersion !== 1 || committed.state !== 'committed'
            || committed.operationId !== id || committed.requestFingerprint !== prepared.requestFingerprint
            || committed.preparedRevision !== hash(Buffer.from(JSON.stringify(prepared)))) {
            throw new Error('LEDGER_STATE_CORRUPT');
        }
        return committed;
    }
    function locked(id, action) {
        const files = paths(id);
        const fd = io.openSync(files.lock, 'wx', 0o600);
        let result, failure;
        const cleanupWarnings = [];
        try {
            result = action(files, cleanupWarnings);
        } catch (error) { failure = error; }
        finally {
            try { io.closeSync(fd); } catch (error) { cleanupWarnings.push(error.message); }
            try { io.unlinkSync(files.lock); } catch (error) { cleanupWarnings.push(error.message); }
        }
        if (failure) {
            failure.cleanupWarnings = [...(failure.cleanupWarnings || []), ...cleanupWarnings];
            throw failure;
        }
        return { record: result, cleanupWarnings };
    }
    function prepare(id, fingerprint, intent) {
        if (!validHash(fingerprint)) throw new Error('OPERATION_FINGERPRINT_INVALID');
        return locked(id, (files, cleanupWarnings) => {
            const previous = query(id);
            if (previous.state !== 'missing') {
                if (previous.requestFingerprint !== fingerprint) throw new Error('OPERATION_ID_PAYLOAD_CONFLICT');
                if (previous.state === 'retired') throw new Error('OPERATION_RETIRED');
                return previous; // caller must reconcile prepared, not execute blindly
            }
            const record = { schemaVersion: 1, operationId: id, state: 'prepared',
                requestFingerprint: fingerprint, intent };
            const publication = publish(files.prepared, record, io);
            cleanupWarnings.push(...publication.cleanupWarnings);
            // No fallible reread after a confirmed publication.
            return JSON.parse(JSON.stringify(record));
        });
    }
    function commit(id, fingerprint, receipt) {
        return locked(id, (files, cleanupWarnings) => {
            const previous = query(id);
            if (previous.state === 'missing') throw new Error('OPERATION_NOT_PREPARED');
            if (previous.requestFingerprint !== fingerprint) throw new Error('OPERATION_ID_PAYLOAD_CONFLICT');
            if (previous.state === 'committed') return previous;
            if (previous.state === 'retired') throw new Error('OPERATION_RETIRED');
            const record = { schemaVersion: 1, operationId: id, state: 'committed',
                requestFingerprint: fingerprint,
                preparedRevision: hash(Buffer.from(JSON.stringify(previous))), receipt };
            const publication = publish(files.committed, record, io);
            cleanupWarnings.push(...publication.cleanupWarnings);
            return JSON.parse(JSON.stringify(record));
        });
    }
    /**
     * Explicit single-ID compaction, not an expiry policy. Caller must authorize
     * the stored target and supply the fingerprint observed during inspection.
     * Prepared states are never discarded. Tombstones are not automatically purged.
     */
    function retire(id, fingerprint) {
        if (!validHash(fingerprint)) throw new Error('OPERATION_FINGERPRINT_INVALID');
        return locked(id, (files, cleanupWarnings) => {
            let previous = query(id);
            if (previous.state === 'missing') throw new Error('OPERATION_NOT_FOUND');
            if (previous.requestFingerprint !== fingerprint) throw new Error('OPERATION_ID_PAYLOAD_CONFLICT');
            if (previous.state === 'prepared') throw new Error('OPERATION_RECONCILIATION_REQUIRED');
            if (previous.state === 'committed') {
                const target = previous.receipt?.path;
                if (typeof target !== 'string' || !path.isAbsolute(target)) {
                    throw new Error('LEDGER_TARGET_INVALID');
                }
                const retired = { schemaVersion: 1, operationId: id, state: 'retired',
                    requestFingerprint: fingerprint, path: target,
                    committedRevision: hash(Buffer.from(JSON.stringify(previous))) };
                const publication = publish(files.retired, retired, io);
                cleanupWarnings.push(...publication.cleanupWarnings);
                previous = retired;
            }
            // Only after the irreversible no-replay marker exists. Failures are
            // warnings, and a later explicit retire can retry removal safely.
            for (const file of [files.committed, files.prepared]) {
                try { io.unlinkSync(file); }
                catch (error) { if (error.code !== 'ENOENT') cleanupWarnings.push(error.message); }
            }
            return previous;
        });
    }
    return { query, prepare, commit, retire };
}
module.exports = { createLedger };

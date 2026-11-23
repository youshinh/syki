// syki Core v1 API: knowledge / exec / flow (Decision model + CanvasHost detach guard)
// UMD / CommonJS compatible: window.SykiApiFlow / module.exports
(function (global) {
  'use strict';

  const DEFAULT_DECIDE_TIMEOUT_MS = 8000;
  let reqSeq = 0;

  function nextReqId(prefix) {
    reqSeq += 1;
    return `${prefix || 'req'}_${Date.now().toString(36)}_${reqSeq}`;
  }

  function createAbortError(message) {
    const err = new Error(message || 'The operation was aborted.');
    err.name = 'AbortError';
    err.code = 'ABORT_ERR';
    return err;
  }

  function isAbortError(err) {
    return Boolean(err && (err.name === 'AbortError' || err.code === 'ABORT_ERR'));
  }

  function createPolicyError(decision) {
    const action = String((decision && (decision.action || decision.verdict)) || 'deny').toLowerCase();
    const code = action === 'ask' ? 'POLICY_ASK' : 'POLICY_DENIED';
    const reason = (decision && decision.reason) ? `: ${decision.reason}` : '';
    const err = new Error(`Command execution blocked by policy (${code})${reason}`);
    err.name = 'PolicyError';
    err.code = code;
    err.decision = decision;
    return err;
  }

  function createInvalidPathError(relPath, detail) {
    const err = new Error(`Invalid note path "${String(relPath)}": ${detail}`);
    err.name = 'InvalidNotePathError';
    err.code = 'INVALID_NOTE_PATH';
    err.path = relPath;
    return err;
  }

  /**
   * Validates that a note path stays strictly inside the notes root directory.
   * Rejects empty paths (unless allowEmpty), NUL bytes, absolute/UNC paths,
   * Windows drive letters / colons, and '..' traversal segments.
   */
  function validateNotePath(relPath, opts) {
    const allowEmpty = Boolean(opts && opts.allowEmpty);
    if (relPath === undefined || relPath === null) {
      if (allowEmpty) return '';
      throw createInvalidPathError(relPath, 'path is required');
    }
    if (typeof relPath !== 'string') {
      throw createInvalidPathError(relPath, 'path must be a string');
    }
    const trimmed = relPath.trim();
    if (!trimmed) {
      if (allowEmpty) return '';
      throw createInvalidPathError(relPath, 'path must not be empty');
    }
    if (trimmed.indexOf('\0') !== -1) {
      throw createInvalidPathError(relPath, 'NUL byte is not allowed');
    }
    if (trimmed.startsWith('/') || trimmed.startsWith('\\')) {
      throw createInvalidPathError(relPath, 'absolute paths are not allowed');
    }
    if (/^[a-zA-Z]:/.test(trimmed) || trimmed.indexOf(':') !== -1) {
      throw createInvalidPathError(relPath, 'drive letters or colons are not allowed');
    }
    const segments = trimmed.split(/[\\/]+/);
    for (let i = 0; i < segments.length; i += 1) {
      if (segments[i] === '..') {
        throw createInvalidPathError(relPath, 'parent directory traversal ("..") is not allowed');
      }
    }
    return trimmed.replace(/\\/g, '/');
  }

  function validateAssetExt(ext) {
    if (typeof ext !== 'string' || !ext.trim()) {
      const err = new Error('Asset extension must be a non-empty string');
      err.code = 'INVALID_ASSET_EXT';
      throw err;
    }
    const cleaned = ext.trim().replace(/^\.+/, '');
    if (!/^[a-zA-Z0-9_-]{1,16}$/.test(cleaned)) {
      const err = new Error(`Invalid asset extension: "${ext}"`);
      err.code = 'INVALID_ASSET_EXT';
      throw err;
    }
    return cleaned.toLowerCase();
  }

  function redactStringSecrets(text) {
    if (typeof text !== 'string' || !text) return text;
    let out = text;
    // Redact http(s) userinfo credentials: https://user:pass@host -> https://host
    out = out.replace(/(https?:\/\/)([^/\s@]+)@/gi, '$1');
    // Redact secret query parameter values
    out = out.replace(/([?&](?:key|api[_-]?key|token|secret|sig|signature|auth|authorization|password|passwd)=)([^&#\s]+)/gi, '$1[REDACTED]');
    // Redact common provider keys & bearer tokens
    out = out.replace(/\b(sk-[A-Za-z0-9_-]{8,}|AIza[0-9A-Za-z_-]{16,}|gh[pousr]_[A-Za-z0-9]{16,})\b/g, '[REDACTED]');
    out = out.replace(/\b(Bearer\s+)[A-Za-z0-9._-]{12,}/gi, '$1[REDACTED]');
    // Redact key=value / key: value style secrets in prose or env snippets
    out = out.replace(/\b([A-Za-z0-9_.-]*(?:api[_-]?key|token|secret|password|passwd)[A-Za-z0-9_.-]*\s*[:=]\s*["']?)([^\s"',;}{]+)(["']?)/gi, '$1[REDACTED]$3');
    return out;
  }

  function defaultStripSecrets(value) {
    if (typeof value === 'string') {
      return redactStringSecrets(value);
    }
    if (value && typeof value === 'object') {
      if (global && global.SykiSecretStrip && typeof global.SykiSecretStrip.stripSecrets === 'function') {
        return global.SykiSecretStrip.stripSecrets(value);
      }
      if (Array.isArray(value)) {
        return value.map((item) => defaultStripSecrets(item));
      }
      const out = {};
      Object.keys(value).forEach((k) => {
        if (/(?:apikey|api_key|api-key|token|secret|password|passwd)/i.test(k) && typeof value[k] === 'string') {
          out[k] = '[REDACTED]';
        } else {
          out[k] = defaultStripSecrets(value[k]);
        }
      });
      return out;
    }
    return value;
  }

  function applyStripSecrets(deps, value, enabled) {
    if (enabled === false || value === undefined || value === null) {
      return value;
    }
    if (deps && typeof deps.stripSecrets === 'function') {
      return deps.stripSecrets(value);
    }
    return defaultStripSecrets(value);
  }

  function normalizeChoices(choices) {
    if (!Array.isArray(choices)) return [];
    const out = [];
    for (let i = 0; i < choices.length; i += 1) {
      const item = choices[i];
      if (typeof item === 'string' && item.trim()) {
        out.push({ id: item.trim() });
      } else if (item && typeof item === 'object' && typeof item.id === 'string' && item.id.trim()) {
        out.push({
          id: item.id.trim(),
          label: item.label,
          description: item.description,
        });
      }
    }
    return out;
  }

  function extractFromParsedObject(obj, validIds) {
    if (!obj || typeof obj !== 'object') return null;
    const candidates = [obj.choiceId, obj.choice, obj.id];
    for (let i = 0; i < candidates.length; i += 1) {
      if (typeof candidates[i] === 'string') {
        const trimmed = candidates[i].trim();
        if (validIds.has(trimmed)) return trimmed;
      }
    }
    return null;
  }

  /**
   * Strictly extracts a choiceId that exactly matches an entry in validIds.
   * Supports direct strings, fenced markdown blocks (```json ... ```),
   * raw JSON objects, and response envelopes ({ choiceId | text | content }).
   */
  function extractChoiceId(raw, validIds) {
    if (raw === null || raw === undefined || !(validIds instanceof Set) || validIds.size === 0) {
      return null;
    }
    if (typeof raw === 'object') {
      const direct = extractFromParsedObject(raw, validIds);
      if (direct) return direct;
      if (typeof raw.text === 'string') return extractChoiceId(raw.text, validIds);
      if (typeof raw.content === 'string') return extractChoiceId(raw.content, validIds);
      if (typeof raw.output === 'string') return extractChoiceId(raw.output, validIds);
      return null;
    }
    if (typeof raw !== 'string') return null;

    const trimmed = raw.trim();
    if (!trimmed) return null;
    if (validIds.has(trimmed)) return trimmed;

    // 1. Markdown fenced code block (```json ... ``` or ``` ... ```)
    const fenceMatch = trimmed.match(/```(?:json)?\s*([\s\S]*?)```/i);
    if (fenceMatch && fenceMatch[1]) {
      const inner = fenceMatch[1].trim();
      if (validIds.has(inner)) return inner;
      try {
        const parsed = JSON.parse(inner);
        const fromObj = extractFromParsedObject(parsed, validIds);
        if (fromObj) return fromObj;
      } catch (_e) {
        // Ignore parse failure and fall through
      }
    }

    // 2. Direct JSON object string
    try {
      const parsed = JSON.parse(trimmed);
      if (typeof parsed === 'string' && validIds.has(parsed.trim())) {
        return parsed.trim();
      }
      const fromObj = extractFromParsedObject(parsed, validIds);
      if (fromObj) return fromObj;
    } catch (_e) {
      // Ignore parse failure
    }

    // 3. Embedded JSON object substring
    const firstBrace = trimmed.indexOf('{');
    const lastBrace = trimmed.lastIndexOf('}');
    if (firstBrace !== -1 && lastBrace > firstBrace) {
      try {
        const parsed = JSON.parse(trimmed.slice(firstBrace, lastBrace + 1));
        const fromObj = extractFromParsedObject(parsed, validIds);
        if (fromObj) return fromObj;
      } catch (_e) {
        // Ignore parse failure
      }
    }

    // 4. Quoted single token (e.g. `"task"` or `'task'`)
    const unquoted = trimmed.replace(/^["'`]+|["'`]+$/g, '').trim();
    if (validIds.has(unquoted)) return unquoted;

    return null;
  }

  function withDetachGuard(deps, fn) {
    if (deps && typeof deps.acquireDetachGuard === 'function') {
      deps.acquireDetachGuard();
    }
    let result;
    try {
      result = fn();
    } catch (err) {
      if (deps && typeof deps.releaseDetachGuard === 'function') {
        deps.releaseDetachGuard();
      }
      return Promise.reject(err);
    }
    return Promise.resolve(result).finally(() => {
      if (deps && typeof deps.releaseDetachGuard === 'function') {
        deps.releaseDetachGuard();
      }
    });
  }

  function raceWithSignal(promise, signal) {
    if (!signal || typeof signal.addEventListener !== 'function') return promise;
    if (signal.aborted) {
      return Promise.reject(createAbortError());
    }
    return new Promise((resolve, reject) => {
      const onAbort = () => reject(createAbortError());
      signal.addEventListener('abort', onAbort, { once: true });
      Promise.resolve(promise).then(
        (val) => {
          signal.removeEventListener('abort', onAbort);
          resolve(val);
        },
        (err) => {
          signal.removeEventListener('abort', onAbort);
          reject(err);
        }
      );
    });
  }

  function createLinkedAbort(externalSignal, timeoutMs) {
    const controller = typeof AbortController !== 'undefined' ? new AbortController() : null;
    let timedOut = false;
    let timerId = null;

    const onExternalAbort = () => {
      if (controller && !controller.signal.aborted) {
        controller.abort();
      }
    };

    if (externalSignal) {
      if (externalSignal.aborted) {
        onExternalAbort();
      } else if (typeof externalSignal.addEventListener === 'function') {
        externalSignal.addEventListener('abort', onExternalAbort, { once: true });
      }
    }

    if (typeof timeoutMs === 'number' && timeoutMs > 0) {
      timerId = setTimeout(() => {
        timedOut = true;
        if (controller && !controller.signal.aborted) {
          controller.abort();
        }
      }, timeoutMs);
    }

    return {
      signal: controller ? controller.signal : externalSignal,
      isTimedOut: () => timedOut,
      cleanup: () => {
        if (timerId !== null) {
          clearTimeout(timerId);
          timerId = null;
        }
        if (externalSignal) {
          externalSignal.removeEventListener('abort', onExternalAbort);
        }
      },
    };
  }

  function requireDep(deps, name, fn) {
    if (typeof fn !== 'function') {
      const err = new Error(`Dependency "${name}" is not configured`);
      err.code = 'MISSING_DEPENDENCY';
      throw err;
    }
    return fn;
  }

  /**
   * Creates the Core v1 `knowledge` / `exec` / `flow` API facade.
   * Performs zero network probes on construction (0ms startup contract).
   */
  function createSykiFlowApi(deps) {
    const d = deps || {};
    const notesDeps = d.notes || {};
    const assetsDeps = d.assets || {};

    const knowledge = {
      searchSemantic(query, opts) {
        const fn = requireDep(d, 'searchSemantic', d.searchSemantic);
        return Promise.resolve(fn(query, opts || {}));
      },
      searchFulltext(query, opts) {
        const fn = requireDep(d, 'searchFulltext', d.searchFulltext);
        return Promise.resolve(fn(query, opts || {}));
      },
      updateSemanticIndex() {
        const fn = requireDep(d, 'updateSemanticIndex', d.updateSemanticIndex);
        return Promise.resolve(fn());
      },
      notes: {
        read(relPath) {
          try {
            const safePath = validateNotePath(relPath);
            const fn = requireDep(d, 'notes.read', notesDeps.read);
            return Promise.resolve(fn(safePath));
          } catch (err) {
            return Promise.reject(err);
          }
        },
        list(opts) {
          try {
            const options = opts ? Object.assign({}, opts) : {};
            if (options.dir !== undefined) {
              options.dir = validateNotePath(options.dir, { allowEmpty: true });
            }
            if (options.prefix !== undefined) {
              options.prefix = validateNotePath(options.prefix, { allowEmpty: true });
            }
            const fn = requireDep(d, 'notes.list', notesDeps.list);
            return Promise.resolve(fn(options));
          } catch (err) {
            return Promise.reject(err);
          }
        },
        create(relPathOrOpts, content, opts) {
          try {
            const fn = requireDep(d, 'notes.create', notesDeps.create);
            if (relPathOrOpts && typeof relPathOrOpts === 'object') {
              const safeTitle = validateNotePath(relPathOrOpts.title);
              const safeFolder = relPathOrOpts.folder !== undefined
                ? validateNotePath(relPathOrOpts.folder, { allowEmpty: true })
                : undefined;
              return Promise.resolve(fn({
                title: safeTitle,
                content: relPathOrOpts.content !== undefined ? String(relPathOrOpts.content) : '',
                folder: safeFolder,
              }));
            }
            const safePath = validateNotePath(relPathOrOpts);
            return Promise.resolve(fn(safePath, content, opts || {}));
          } catch (err) {
            return Promise.reject(err);
          }
        },
        append(relPath, content, opts) {
          try {
            const safePath = validateNotePath(relPath);
            const fn = requireDep(d, 'notes.append', notesDeps.append);
            return Promise.resolve(fn(safePath, content, opts || {}));
          } catch (err) {
            return Promise.reject(err);
          }
        },
        update(relPath, content, opts) {
          try {
            const safePath = validateNotePath(relPath);
            const fn = requireDep(d, 'notes.update', notesDeps.update);
            return Promise.resolve(fn(safePath, content, opts || {}));
          } catch (err) {
            return Promise.reject(err);
          }
        },
      },
      assets: {
        save(blob, ext) {
          try {
            const safeExt = validateAssetExt(ext);
            const fn = requireDep(d, 'assets.save', assetsDeps.save);
            return Promise.resolve(fn(blob, safeExt));
          } catch (err) {
            return Promise.reject(err);
          }
        },
      },
    };

    const exec = {
      llm(prompt, opts) {
        const options = opts || {};
        const {
          input,
          system,
          stripSecrets = true,
          signal,
          timeoutMs,
          model,
          reqId,
        } = options;

        return withDetachGuard(d, async () => {
          if (signal && signal.aborted) {
            throw createAbortError();
          }
          const fn = requireDep(d, 'queryLlm', d.queryLlm);
          const cleanPrompt = applyStripSecrets(d, prompt, stripSecrets);
          const cleanInput = applyStripSecrets(d, input, stripSecrets);
          const cleanSystem = applyStripSecrets(d, system, stripSecrets);
          const linked = createLinkedAbort(signal, timeoutMs);
          try {
            const callPromise = Promise.resolve(
              fn({
                prompt: cleanPrompt,
                input: cleanInput,
                system: cleanSystem,
                model,
                reqId: reqId || nextReqId('llm'),
                signal: linked.signal,
              })
            );
            return await raceWithSignal(callPromise, linked.signal);
          } catch (err) {
            if (linked.isTimedOut() && !signal?.aborted) {
              const timeoutErr = new Error(`LLM request timed out after ${timeoutMs}ms`);
              timeoutErr.code = 'TIMEOUT';
              throw timeoutErr;
            }
            throw err;
          } finally {
            linked.cleanup();
          }
        });
      },

      shell(program, args, opts) {
        const argList = args === undefined ? [] : args;
        const options = opts || {};
        const { stdin, cwd, timeoutMs, signal } = options;

        return withDetachGuard(d, async () => {
          if (signal && signal.aborted) {
            throw createAbortError();
          }
          if (typeof program !== 'string' || !program.trim()) {
            const err = new TypeError('exec.shell requires a non-empty program string');
            err.code = 'INVALID_ARGUMENT';
            throw err;
          }
          if (!Array.isArray(argList)) {
            const err = new TypeError('exec.shell args must be an array of strings');
            err.code = 'INVALID_ARGUMENT';
            throw err;
          }
          const fn = requireDep(d, 'runProgram', d.runProgram);
          const res = await raceWithSignal(
            Promise.resolve(fn(program, argList.slice(), { stdin, cwd, timeoutMs, signal })),
            signal
          );
          const decision = res && (res.decision || (typeof res.allowed === 'boolean' ? res : null));
          const blocked = Boolean(
            res && (
              res.allowed === false
              || (decision && (
                decision.allowed === false
                || String(decision.verdict || decision.action || '').toLowerCase() === 'deny'
                || String(decision.verdict || decision.action || '').toLowerCase() === 'ask'
              ))
            )
          );
          if (blocked) {
            throw createPolicyError(decision || res);
          }
          return res;
        });
      },

      pty: {
        open(opts) {
          return withDetachGuard(d, async () => {
            const options = opts || {};
            if (options.signal && options.signal.aborted) {
              throw createAbortError();
            }
            const fn = requireDep(d, 'openPty', d.openPty);
            return await fn(options);
          });
        },
      },
    };

    const flow = {
      steps(initial, stepList, opts) {
        const options = opts || {};
        const { progress, signal } = options;

        return withDetachGuard(d, async () => {
          if (!Array.isArray(stepList)) {
            throw new TypeError('flow.steps requires an array of steps');
          }
          let current = initial;
          for (let i = 0; i < stepList.length; i += 1) {
            if (signal && signal.aborted) {
              throw createAbortError();
            }
            const step = stepList[i];
            const label = (step && (step.label || step.name)) || `step_${i + 1}`;
            if (progress && typeof progress.update === 'function') {
              progress.update(label, { index: i, total: stepList.length });
            }
            const runFn = typeof step === 'function' ? step : step && step.run;
            if (typeof runFn !== 'function') {
              throw new TypeError(`flow.steps: step at index ${i} is not executable`);
            }
            const effectiveSignal = signal || (typeof AbortController !== 'undefined' ? new AbortController().signal : undefined);
            current = await raceWithSignal(
              Promise.resolve(runFn(current, effectiveSignal)),
              effectiveSignal
            );
            if (signal && signal.aborted) {
              throw createAbortError();
            }
          }
          return current;
        });
      },

      parallel(tasks) {
        return withDetachGuard(d, async () => {
          if (!Array.isArray(tasks)) {
            throw new TypeError('flow.parallel requires an array of tasks');
          }
          const promises = tasks.map((task) => (typeof task === 'function' ? Promise.resolve().then(task) : Promise.resolve(task)));
          return await Promise.all(promises);
        });
      },

      decide(opts) {
        const options = opts || {};
        const {
          input,
          choices,
          rule,
          engine = 'decision',
          prompt,
          stripSecrets = true,
          fallback,
          signal,
          timeoutMs = DEFAULT_DECIDE_TIMEOUT_MS,
        } = options;

        return withDetachGuard(d, async () => {
          // 1. Build valid choice ID set
          const normalizedChoices = normalizeChoices(choices);
          const validIds = new Set(normalizedChoices.map((c) => c.id));

          // 2. Rule-first short-circuit (0ms, zero network)
          if (typeof rule === 'function') {
            const ruleResult = await rule(input);
            if (typeof ruleResult === 'string' && validIds.has(ruleResult.trim())) {
              return { choiceId: ruleResult.trim(), by: 'rule' };
            }
          }

          // 3. Respect caller cancellation immediately
          if (signal && signal.aborted) {
            throw createAbortError();
          }

          // 4. Mask secrets by default before sending to any model
          const cleanInput = applyStripSecrets(d, input, stripSecrets);
          const cleanPrompt = applyStripSecrets(d, prompt, stripSecrets);
          const reqId = nextReqId('decide');
          const linked = createLinkedAbort(signal, timeoutMs);

          let lastReason = 'no_decision_engine_available';

          const runDecisionCall = async () => {
            const res = await raceWithSignal(
              Promise.resolve(
                d.queryDecision({
                  reqId,
                  input: cleanInput,
                  prompt: cleanPrompt,
                  choices: normalizedChoices,
                  signal: linked.signal,
                })
              ),
              linked.signal
            );
            if (res && typeof res === 'object' && res.reqId !== undefined && res.reqId !== reqId) {
              lastReason = `reqId_mismatch:${res.reqId}`;
              return null;
            }
            const extracted = extractChoiceId(res, validIds);
            if (extracted) {
              return {
                choiceId: extracted,
                by: 'decision',
                reason: (res && typeof res === 'object' && res.reason) || undefined,
                reqId,
              };
            }
            lastReason = 'invalid_decision_choice';
            return null;
          };

          const runLlmFallbackCall = async () => {
            const choiceIdsText = normalizedChoices.map((c) => c.id).join(', ');
            const fallbackPrompt = cleanPrompt
              ? `${cleanPrompt}\nReturn ONLY one exact choice ID from: [${choiceIdsText}]`
              : `Choose exactly one ID from [${choiceIdsText}] for the input.`;
            const res = await raceWithSignal(
              Promise.resolve(
                d.queryLlm({
                  reqId,
                  prompt: fallbackPrompt,
                  input: cleanInput,
                  choices: normalizedChoices,
                  signal: linked.signal,
                })
              ),
              linked.signal
            );
            if (res && typeof res === 'object' && res.reqId !== undefined && res.reqId !== reqId) {
              lastReason = `reqId_mismatch:${res.reqId}`;
              return null;
            }
            const extracted = extractChoiceId(res, validIds);
            if (extracted) {
              return {
                choiceId: extracted,
                by: 'llm',
                reason: (res && typeof res === 'object' && res.reason) || undefined,
                reqId,
              };
            }
            lastReason = 'invalid_llm_choice';
            return null;
          };

          try {
            const decisionDisabled = d.capabilities && d.capabilities.decisionModel === false;
            const canUseDecision = engine === 'decision' && !decisionDisabled && typeof d.queryDecision === 'function';

            if (canUseDecision) {
              try {
                const decisionOut = await runDecisionCall();
                if (decisionOut) return decisionOut;
              } catch (err) {
                if (signal && signal.aborted) {
                  throw createAbortError();
                }
                if (linked.isTimedOut()) {
                  return { choiceId: fallback, reason: 'timeout', by: 'fallback' };
                }
                lastReason = (err && err.message) || 'decision_error';
              }
            } else if (engine === 'decision') {
              lastReason = decisionDisabled ? 'decision_model_disabled' : 'decision_model_unavailable';
            }

            // Fallback to standard LLM when engine === 'llm' or decision model unavailable/failed
            if (typeof d.queryLlm === 'function') {
              if (signal && signal.aborted) {
                throw createAbortError();
              }
              try {
                const llmOut = await runLlmFallbackCall();
                if (llmOut) return llmOut;
              } catch (err) {
                if (signal && signal.aborted) {
                  throw createAbortError();
                }
                if (linked.isTimedOut()) {
                  return { choiceId: fallback, reason: 'timeout', by: 'fallback' };
                }
                lastReason = (err && err.message) || 'llm_error';
              }
            }

            return {
              choiceId: fallback,
              reason: lastReason,
              by: 'fallback',
            };
          } finally {
            linked.cleanup();
          }
        });
      },
    };

    return {
      knowledge,
      exec,
      flow,
    };
  }

  const exported = {
    createSykiFlowApi,
    validateNotePath,
    validateAssetExt,
    extractChoiceId,
    defaultStripSecrets,
    createAbortError,
    isAbortError,
    createPolicyError,
  };

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = exported;
  }
  if (global) {
    global.SykiApiFlow = exported;
  }
})(typeof window !== 'undefined' ? window : globalThis);

package ops

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/jackc/pgx/v5"
)

// Idempotency makes a mutating request safe to repeat.
//
// A caller sends `Idempotency-Key`. The first request with a key runs and its response
// is stored; a replay with the same key and the same body returns the stored response
// and does nothing. A replay with the same key and a *different* body is refused,
// because that is a caller reusing a key for a different operation — executing it
// would be a duplicate in disguise, and silently ignoring the difference would hide a
// bug in their retry logic.
//
// ## Why the row is inserted before the handler runs
//
// The obvious implementation — run the handler, then store the response — has a race:
// two requests with the same key both miss the lookup and both execute. Inserting a
// placeholder first makes the second request collide on the primary key, which is a
// database-level guarantee rather than a timing assumption.
//
// A placeholder that is never completed (the process died mid-request) is treated as
// retryable after `inFlightTimeout`, so a crash does not wedge a key forever.

// storedResponse is what a replay returns.
type storedResponse struct {
	Status int             `json:"status"`
	Body   json.RawMessage `json:"body"`
}

// inFlightTimeout is how long a claimed-but-unfinished key blocks replays. Longer than
// any request the edge allows (the gateway's upstream timeout is 10 s), so a normal
// request never expires its own claim.
const inFlightTimeout = 60 * time.Second

// IdempotencyOutcome is what a caller should do with a request.
type IdempotencyOutcome int

const (
	// IdempotencyProceed means the key was free and this request owns it. The caller
	// runs the handler and then calls Store.
	IdempotencyProceed IdempotencyOutcome = iota
	// IdempotencyReplay means the key completed earlier; return the stored response.
	IdempotencyReplay
)

// ClaimIdempotency attempts to take ownership of an idempotency key.
//
// Returns IdempotencyReplay with the stored response when the key already completed
// with the same request body, ErrIdempotencyConflict when it completed with a different
// body, ErrIdempotencyInFlight when another request holds it, and IdempotencyProceed
// when this caller now owns it and must call Store afterwards.
func (s *Store) ClaimIdempotency(ctx context.Context, scope, key, requestHash string, ttl time.Duration) (IdempotencyOutcome, *storedResponse, error) {
	if scope == "" || key == "" {
		return IdempotencyProceed, nil, fmt.Errorf("ops: idempotency needs a scope and a key")
	}
	if ttl <= 0 {
		ttl = 24 * time.Hour
	}

	// Try to take the key. The placeholder is a zero response with status 0, which no
	// real response can have, so "claimed but unfinished" is distinguishable from
	// "completed with an empty body".
	tag, err := s.pool.Exec(ctx, `
		INSERT INTO ops.idempotency_record (scope, key, request_hash, response, status, expires_at)
		VALUES ($1, $2, $3, 'null'::jsonb, 0, $4)
		ON CONFLICT (scope, key) DO NOTHING`,
		scope, key, requestHash, s.now().UTC().Add(ttl))
	if err != nil {
		return IdempotencyProceed, nil, fmt.Errorf("ops: claiming idempotency key: %w", err)
	}
	if tag.RowsAffected() == 1 {
		return IdempotencyProceed, nil, nil
	}

	// The key exists. Work out whether it finished, and whether it is ours to replay.
	var storedHash string
	var status int
	var body json.RawMessage
	var createdAt time.Time
	err = s.pool.QueryRow(ctx, `
		SELECT request_hash, status, response, created_at
		FROM ops.idempotency_record WHERE scope = $1 AND key = $2`,
		scope, key).Scan(&storedHash, &status, &body, &createdAt)
	if err != nil {
		if isNoRows(err) {
			// Deleted between the insert and the read — an expiring record. The caller
			// may retry; treating it as a normal proceed would risk a duplicate.
			return IdempotencyProceed, nil, ErrIdempotencyInFlight
		}
		return IdempotencyProceed, nil, fmt.Errorf("ops: reading idempotency record: %w", err)
	}

	if storedHash != requestHash {
		return IdempotencyProceed, nil, fmt.Errorf("%w: %s/%s", ErrIdempotencyConflict, scope, key)
	}

	if status == 0 {
		// Claimed but not finished.
		if s.now().Sub(createdAt) < inFlightTimeout {
			return IdempotencyProceed, nil, ErrIdempotencyInFlight
		}
		// The claimant died or vanished. Take the key over rather than blocking the
		// caller forever on a request that will never complete.
		if _, err := s.pool.Exec(ctx, `
			UPDATE ops.idempotency_record
			SET created_at = now(), expires_at = $3
			WHERE scope = $1 AND key = $2 AND status = 0`,
			scope, key, s.now().UTC().Add(ttl)); err != nil {
			return IdempotencyProceed, nil, fmt.Errorf("ops: taking over a stale idempotency claim: %w", err)
		}
		return IdempotencyProceed, nil, nil
	}

	return IdempotencyReplay, &storedResponse{Status: status, Body: body}, nil
}

// SaveIdempotentResponse records the response for a key this caller claimed, so a later replay returns
// it instead of executing again.
func (s *Store) SaveIdempotentResponse(ctx context.Context, scope, key string, status int, body []byte) error {
	if !json.Valid(body) {
		// A non-JSON response is stored as a JSON string so the column stays jsonb. The
		// alternative — a text column — would let a replay return bytes that were never
		// validated.
		encoded, err := json.Marshal(string(body))
		if err != nil {
			return fmt.Errorf("ops: encoding a non-JSON idempotent response: %w", err)
		}
		body = encoded
	}
	tag, err := s.pool.Exec(ctx, `
		UPDATE ops.idempotency_record SET response = $3, status = $4
		WHERE scope = $1 AND key = $2 AND status = 0`,
		scope, key, body, status)
	if err != nil {
		return fmt.Errorf("ops: storing an idempotent response: %w", err)
	}
	if tag.RowsAffected() == 0 {
		return fmt.Errorf("%w: %s/%s is not claimed by this request", ErrNoRun, scope, key)
	}
	return nil
}

// ReleaseIdempotency gives a key back when the handler failed in a way the caller may retry.
//
// Without it, a 500 would be stored as the response and every retry would replay the
// failure. A response worth replaying is a successful one; anything else releases the
// key so a retry genuinely re-runs.
func (s *Store) ReleaseIdempotency(ctx context.Context, scope, key string) error {
	_, err := s.pool.Exec(ctx,
		`DELETE FROM ops.idempotency_record WHERE scope = $1 AND key = $2 AND status = 0`,
		scope, key)
	if err != nil {
		return fmt.Errorf("ops: releasing an idempotency key: %w", err)
	}
	return nil
}

// HashRequest returns the hash that identifies a request body for idempotency purposes.
//
// It is the SHA-256 of the exact bytes. Hashing a re-serialised body would make a
// semantically identical request with different key order look different, which would
// turn a legitimate retry into a conflict.
func HashRequest(body []byte) string {
	sum := sha256.Sum256(body)
	return hex.EncodeToString(sum[:])
}

// IdempotencyKeyHeader is the header name the contract documents.
const IdempotencyKeyHeader = "Idempotency-Key"

// Middleware wraps a handler with idempotency.
//
// Responses are buffered, because a response can only be stored once it is complete —
// a streamed body would be replayed truncated. That is acceptable here for the reason
// the edge accepts a 2 MiB body limit: the endpoints that take an Idempotency-Key
// return small JSON documents. An endpoint that streams should not take one.
//
// Failures release the key rather than storing them; see Release.
func (s *Store) Middleware(scope string) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			key := r.Header.Get(IdempotencyKeyHeader)
			if key == "" {
				// No key: the endpoint documents it as required and a handler above this
				// one enforces that. Passing through keeps this middleware from being a
				// second place that decides what is required.
				next.ServeHTTP(w, r)
				return
			}

			body, err := io.ReadAll(r.Body)
			if err != nil {
				http.Error(w, "reading request body", http.StatusBadRequest)
				return
			}
			_ = r.Body.Close()
			hash := HashRequest(body)
			// Restore the body for the handler; reading it consumed it.
			r.Body = io.NopCloser(bytesReader(body))

			outcome, stored, err := s.ClaimIdempotency(r.Context(), scope, key, hash, 0)
			switch {
			case errors.Is(err, ErrIdempotencyConflict):
				writeProblem(w, http.StatusUnprocessableEntity,
					"about:blank#idempotency-key-reused",
					"Idempotency-Key reused with a different body")
				return
			case errors.Is(err, ErrIdempotencyInFlight):
				w.Header().Set("Retry-After", "1")
				writeProblem(w, http.StatusConflict,
					"about:blank#conflict",
					"An identical request is already in flight")
				return
			case err != nil:
				writeProblem(w, http.StatusInternalServerError,
					"about:blank#internal", "Idempotency bookkeeping failed")
				return
			}

			if outcome == IdempotencyReplay && stored != nil {
				w.Header().Set("Content-Type", "application/json; charset=utf-8")
				w.Header().Set("Idempotent-Replayed", "true")
				w.WriteHeader(stored.Status)
				_, _ = w.Write(stored.Body)
				return
			}

			recorder := &bufferedResponse{header: make(http.Header), status: http.StatusOK}
			next.ServeHTTP(recorder, r)

			if recorder.status >= 500 {
				// Do not store a server error: the key is released so a retry re-runs.
				if err := s.ReleaseIdempotency(r.Context(), scope, key); err != nil {
					// Logged by the caller's middleware chain; the response is already
					// decided and a release failure must not change it.
					_ = err
				}
			} else if err := s.SaveIdempotentResponse(r.Context(), scope, key, recorder.status, recorder.body); err != nil {
				_ = err
			}

			for name, values := range recorder.header {
				for _, value := range values {
					w.Header().Add(name, value)
				}
			}
			w.WriteHeader(recorder.status)
			_, _ = w.Write(recorder.body)
		})
	}
}

// bufferedResponse records what the handler wrote so it can be stored and then replayed
// verbatim.
type bufferedResponse struct {
	header http.Header
	status int
	body   []byte
}

func (b *bufferedResponse) Header() http.Header { return b.header }

func (b *bufferedResponse) WriteHeader(status int) { b.status = status }

func (b *bufferedResponse) Write(data []byte) (int, error) {
	b.body = append(b.body, data...)
	return len(data), nil
}

// writeProblem writes a minimal RFC 9457 response.
//
// The gateway owns the full problem writer; this is the library's own small one, used
// only when idempotency itself fails before the handler runs. It carries the same
// content type and the same `type` vocabulary so a client parses it with the same code.
func writeProblem(w http.ResponseWriter, status int, problemType, title string) {
	body, _ := json.Marshal(map[string]any{
		"type":   problemType,
		"title":  title,
		"status": status,
	})
	w.Header().Set("Content-Type", "application/problem+json")
	w.WriteHeader(status)
	_, _ = w.Write(body)
}

// bytesReader returns an io.ReadCloser over b.
func bytesReader(b []byte) io.ReadCloser {
	return io.NopCloser(&sliceReader{data: b})
}

// sliceReader is a minimal reader so the middleware does not need bytes.Buffer.
type sliceReader struct {
	data []byte
	at   int
}

func (r *sliceReader) Read(p []byte) (int, error) {
	if r.at >= len(r.data) {
		return 0, io.EOF
	}
	n := copy(p, r.data[r.at:])
	r.at += n
	return n, nil
}

// keep pgx referenced for the transaction helper used above.
var _ = pgx.ErrNoRows

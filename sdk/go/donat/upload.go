package donat

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

// Uploader stores bytes a function produced — a rendered PDF, a generated
// report — as a file the engine's attachment machinery owns.
//
// It is reached from inside a function through the request's context:
//
//	func renderInvoicePDF(ctx context.Context, a Args) (Out, error) {
//	    pdf := render(a.InvoiceID)
//	    id, err := donat.UploaderFrom(ctx).Upload(ctx,
//	        "public.invoice.pdf", "invoice.pdf", "application/pdf", pdf)
//	    return Out{FileID: id}, err
//	}
//
// The returned id is a `pending` upload. It becomes the column's value when a
// mutation writes it there, and the claim gate in that same statement is what
// certifies it — so a file can never be referenced by a row without having been
// stored and measured first.
//
// The permission check is the ordinary one: minting runs as the caller's
// session, so a role that may not write the file column cannot obtain an
// upload for it either.
type Uploader interface {
	Upload(ctx context.Context, attachment, fileName, mediaType string, content []byte) (string, error)
}

type uploaderKey struct{}

// UploaderFrom returns the Uploader for this request. It is never nil inside a
// function the engine called; outside one it returns an Uploader that explains
// itself rather than panicking.
func UploaderFrom(ctx context.Context) Uploader {
	if u, ok := ctx.Value(uploaderKey{}).(Uploader); ok && u != nil {
		return u
	}
	return unavailableUploader{}
}

type unavailableUploader struct{}

func (unavailableUploader) Upload(context.Context, string, string, string, []byte) (string, error) {
	return "", fmt.Errorf(
		"donat: no uploader in this context — Upload is available inside a function the " +
			"engine called, and only when the metadata declares an attachment")
}

// engineUploader is the real one, bound to the request's session so that
// minting is subject to the caller's permissions.
type engineUploader struct {
	engine      *Engine
	sessionVars map[string]string
}

// Upload mints an upload, stores the bytes, and finishes it.
//
// The bytes go straight to the object store with a URL the core signed; they
// never pass through the engine. Finishing asks the store what it actually
// received rather than trusting the size that was declared, and moves the
// object out from under the URL that wrote it — that URL stays valid for its
// whole life and cannot be revoked, so bytes left at the address it writes to
// could be replaced after a claim had certified them.
func (u engineUploader) Upload(
	ctx context.Context,
	attachment, fileName, mediaType string,
	content []byte,
) (string, error) {
	if len(content) == 0 {
		return "", fmt.Errorf("donat: refusing to store an empty file for %q", attachment)
	}

	minted, err := u.mint(ctx, attachment, fileName, mediaType, len(content))
	if err != nil {
		return "", err
	}
	if err := u.store(ctx, minted, content); err != nil {
		return "", err
	}
	if err := u.engine.finishUpload(ctx, minted.ID, attachment, int64(len(content))); err != nil {
		return "", err
	}
	return minted.ID, nil
}

// mintedUpload is the part of donat_request_file_upload's answer this needs.
type mintedUpload struct {
	ID      string
	URL     string
	Method  string
	Headers []struct{ Name, Value string }
}

// mint runs the ordinary upload mutation as the caller, so the declaration's
// media types, size ceiling and per-session budgets all apply — and so does the
// role's permission to write the column.
func (u engineUploader) mint(
	ctx context.Context,
	attachment, fileName, mediaType string,
	size int,
) (mintedUpload, error) {
	const query = `mutation ($a: donat_file_attachment!, $n: String!, $m: String!, $s: Int!) {
		donat_request_file_upload(attachment: $a, file_name: $n, media_type: $m, size: $s) {
			id url method headers { name value }
		}
	}`
	vars := map[string]json.RawMessage{
		"a": mustJSON(attachment),
		"n": mustJSON(fileName),
		"m": mustJSON(mediaType),
		"s": mustJSON(size),
	}
	body, err := u.engine.Execute(ctx, query, vars, u.sessionVars)
	if err != nil {
		return mintedUpload{}, fmt.Errorf("donat: requesting an upload: %w", err)
	}
	var answer struct {
		Data struct {
			Upload mintedUpload `json:"donat_request_file_upload"`
		} `json:"data"`
		Errors []struct {
			Message string `json:"message"`
		} `json:"errors"`
	}
	if err := json.Unmarshal(body, &answer); err != nil {
		return mintedUpload{}, fmt.Errorf("donat: decoding the upload request: %w", err)
	}
	if len(answer.Errors) > 0 {
		// The engine refused — an unknown attachment, a media type the
		// declaration does not accept, a size over its ceiling, or a role that
		// may not write the column. That is the caller's answer.
		return mintedUpload{}, fmt.Errorf("donat: %s", answer.Errors[0].Message)
	}
	if answer.Data.Upload.ID == "" || answer.Data.Upload.URL == "" {
		return mintedUpload{}, fmt.Errorf("donat: the engine minted no upload for %q", attachment)
	}
	return answer.Data.Upload, nil
}

// store PUTs the bytes with the presigned URL and the headers it was signed
// with. Changing either invalidates the signature, which is the point.
func (u engineUploader) store(ctx context.Context, minted mintedUpload, content []byte) error {
	method := minted.Method
	if method == "" {
		method = http.MethodPut
	}
	req, err := http.NewRequestWithContext(ctx, method, minted.URL, bytes.NewReader(content))
	if err != nil {
		return fmt.Errorf("donat: storing an upload: %w", err)
	}
	for _, h := range minted.Headers {
		req.Header.Set(h.Name, h.Value)
	}
	resp, err := u.engine.httpClient().Do(req)
	if err != nil {
		return fmt.Errorf("donat: the object store did not answer: %w", err)
	}
	defer resp.Body.Close() //nolint:errcheck
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		detail, _ := io.ReadAll(io.LimitReader(resp.Body, 512))
		return fmt.Errorf("donat: the object store refused the upload (%s): %s",
			resp.Status, bytes.TrimSpace(detail))
	}
	return nil
}

// completionRow is what the engine must read back before it can finish an
// upload: the core signs against the address the bytes were actually written
// to, which the mutation chose.
type completionRow struct {
	Backend    string
	StagingKey string
}

// finishUpload verifies the stored object and moves it to its final address.
func (e *Engine) finishUpload(ctx context.Context, id, attachment string, declared int64) error {
	row, err := e.backend.ReadUpload(ctx, id)
	if err != nil {
		return fmt.Errorf("donat: reading the upload row: %w", err)
	}

	signed, err := e.signCompletion(ctx, completionInput{
		UploadID:   id,
		Attachment: attachment,
		Backend:    row.Backend,
		StagingKey: row.StagingKey,
		Now:        e.now().UTC().Format(time.RFC3339),
	})
	if err != nil {
		return err
	}

	// Ask the store what it holds. A size the caller merely promised would let
	// a claim succeed for an object that was never written.
	size, err := e.observedSize(ctx, signed.HeadURL)
	if err != nil {
		return err
	}
	if size != declared {
		return fmt.Errorf(
			"donat: the object store holds %d bytes for the upload, not the %d that were sent",
			size, declared)
	}
	if signed.MaxBytes > 0 && uint64(size) > signed.MaxBytes {
		return fmt.Errorf("donat: the stored object exceeds the declaration's %d-byte ceiling",
			signed.MaxBytes)
	}

	if err := e.copyObject(ctx, signed); err != nil {
		return err
	}
	// Point the row at the final key *before* dropping the staging object: the
	// other order loses the file to a crash in between, leaving a row naming an
	// object that no longer exists. The worst case this way is one stale
	// staging object, which the collector reclaims.
	if err := e.backend.FinalizeUpload(ctx, id, signed.FinalKey, size); err != nil {
		return fmt.Errorf("donat: recording the upload: %w", err)
	}
	e.deleteObject(ctx, signed.DeleteURL)
	return nil
}

func (e *Engine) observedSize(ctx context.Context, headURL string) (int64, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodHead, headURL, nil)
	if err != nil {
		return 0, fmt.Errorf("donat: checking the stored object: %w", err)
	}
	resp, err := e.httpClient().Do(req)
	if err != nil {
		return 0, fmt.Errorf("donat: the object store did not answer: %w", err)
	}
	defer resp.Body.Close() //nolint:errcheck
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		return 0, fmt.Errorf("donat: the object was not stored (%s)", resp.Status)
	}
	// A HEAD response has no body, so the header is the only size there is.
	if resp.ContentLength < 0 {
		return 0, fmt.Errorf("donat: the object store reported no size")
	}
	return resp.ContentLength, nil
}

func (e *Engine) copyObject(ctx context.Context, signed completionUrls) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodPut, signed.CopyURL, nil)
	if err != nil {
		return fmt.Errorf("donat: finalizing the upload: %w", err)
	}
	for _, h := range signed.CopyHeaders {
		if len(h) == 2 {
			req.Header.Set(h[0], h[1])
		}
	}
	resp, err := e.httpClient().Do(req)
	if err != nil {
		return fmt.Errorf("donat: the object store did not answer: %w", err)
	}
	defer resp.Body.Close() //nolint:errcheck
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		return fmt.Errorf("donat: the object store refused to finalize the upload (%s)", resp.Status)
	}
	return nil
}

// deleteObject drops the staging copy. A failure here leaves an orphan the
// collector reclaims, so it is logged by its absence rather than returned — the
// file itself is already safe.
func (e *Engine) deleteObject(ctx context.Context, deleteURL string) {
	req, err := http.NewRequestWithContext(ctx, http.MethodDelete, deleteURL, nil)
	if err != nil {
		return
	}
	if resp, err := e.httpClient().Do(req); err == nil {
		_ = resp.Body.Close()
	}
}

// completionInput mirrors crates/wasm-core/src/compile.rs CompletionInput.
type completionInput struct {
	UploadID   string `json:"upload_id"`
	Attachment string `json:"attachment"`
	Backend    string `json:"backend"`
	StagingKey string `json:"staging_key"`
	Now        string `json:"now"`
}

// completionUrls mirrors CompletionUrls. The signing stays in the core because
// `donat-storage` owns it and has a twin in SQL that conformance pins against a
// real MinIO; a third implementation here would be a third thing to keep right.
type completionUrls struct {
	HeadURL     string     `json:"head_url"`
	CopyURL     string     `json:"copy_url"`
	CopyHeaders [][]string `json:"copy_headers"`
	DeleteURL   string     `json:"delete_url"`
	FinalKey    string     `json:"final_key"`
	MaxBytes    uint64     `json:"max_bytes"`
}

func (e *Engine) signCompletion(ctx context.Context, in completionInput) (completionUrls, error) {
	payload, err := json.Marshal(in)
	if err != nil {
		return completionUrls{}, fmt.Errorf("donat: encoding the completion request: %w", err)
	}
	c, err := e.acquire(ctx)
	if err != nil {
		return completionUrls{}, err
	}
	defer e.release(c)

	raw, err := c.fileCompletion(ctx, payload)
	if err != nil {
		return completionUrls{}, fmt.Errorf("donat: signing the completion: %w", err)
	}
	var answer struct {
		Kind    string `json:"kind"`
		Message string `json:"message"`
		completionUrls
	}
	if err := json.Unmarshal(raw, &answer); err != nil {
		return completionUrls{}, fmt.Errorf("donat: decoding the completion: %w", err)
	}
	if answer.Kind != "urls" {
		return completionUrls{}, fmt.Errorf("donat: %s", answer.Message)
	}
	return answer.completionUrls, nil
}

func mustJSON(v any) json.RawMessage {
	raw, err := json.Marshal(v)
	if err != nil {
		// Only strings and ints reach this.
		return json.RawMessage("null")
	}
	return raw
}

package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"github.com/donatlabs/donat/sdk/go/donat"
)

// Rendering a borrowing receipt — the shape of work that cannot be declared.
//
// Everything else in this example is metadata: who may borrow, how many loans
// a member may hold, what an extension costs. None of it is Go, and none of it
// should be. A PDF is different — it is a library call over bytes, and no
// amount of YAML expresses one.
//
// So it is an action. The declaration lives in `metadata/actions.yaml` with no
// `handler:`, which is what makes it resolved here rather than over a webhook.
//
//	metadata/actions.yaml     the field, its arguments, its result type, its roles
//	metadata/…/public_loan.yaml   the column that holds the file
//	metadata/storage.yaml     where the bytes live and what signs their URLs
//	receipt.go                this — the part that is genuinely code
//
// The bytes never pass through the engine. `Upload` mints a presigned PUT,
// stores them, asks the store what it actually received, and returns a
// `pending` upload id. That id becomes the loan's `receipt` when the mutation
// below writes it, and the claim gate in that same statement is what certifies
// it — a row can never point at bytes nobody stored.

// ReceiptArgs mirrors the action's declared arguments. `donat codegen go`
// generates this shape, and the engine checks it against the metadata at
// startup: a json tag that disagrees would decode to the zero value and answer
// 200 with an empty field.
type ReceiptArgs struct {
	LoanID string `json:"loan_id"`
}

// LoanReceipt mirrors the declared `output_type`. What the function returns is
// validated against it — a field declared `uuid!` cannot come back empty.
type LoanReceipt struct {
	FileID string `json:"file_id"`
	Bytes  int    `json:"bytes"`
}

// Receipts renders and stores borrowing receipts.
//
// It holds the engine because the function reads the loan back through
// GraphQL, and the engine does not exist until every function is registered —
// registration is what its startup check verifies. So the field is filled in
// once, immediately after `donat.New`, rather than captured in a closure that
// could not see it yet.
type Receipts struct {
	engine *donat.Engine
}

// Bind gives the service the engine it reads through.
func (r *Receipts) Bind(engine *donat.Engine) { r.engine = engine }

// Render renders the receipt for a loan and stores it.
func (r *Receipts) Render(ctx context.Context, args ReceiptArgs) (LoanReceipt, error) {
	// Read the loan through GraphQL rather than SQL, so the member's own
	// select permission decides what is visible. A member asking for another
	// member's receipt gets nothing here, rather than a permission error
	// somewhere further along.
	loan, err := r.loan(ctx, args.LoanID)
	if err != nil {
		return LoanReceipt{}, err
	}

	pdf := renderReceiptPDF(loan)

	// Storing runs as the caller: a role that may not write `loan.receipt`
	// cannot obtain an upload for it either.
	fileID, err := donat.UploaderFrom(ctx).Upload(ctx,
		"public.loan.receipt",
		fmt.Sprintf("receipt-%s.pdf", args.LoanID),
		"application/pdf",
		pdf,
	)
	if err != nil {
		return LoanReceipt{}, err
	}
	return LoanReceipt{FileID: fileID, Bytes: len(pdf)}, nil
}

// loan reads the one loan the receipt describes, as the caller.
func (r *Receipts) loan(ctx context.Context, id string) (receiptLoan, error) {
	const query = `query ($id: uuid!) {
		loan(where: { id: { _eq: $id } }, limit: 1) {
			id borrowed_on due_on
			copy { book { title } }
		}
	}`
	body, err := r.engine.Execute(ctx, query,
		map[string]json.RawMessage{"id": mustJSON(id)},
		donat.SessionFromContext(ctx))
	if err != nil {
		return receiptLoan{}, fmt.Errorf("reading the loan: %w", err)
	}

	var answer struct {
		Data struct {
			Loan []struct {
				ID         string `json:"id"`
				BorrowedOn string `json:"borrowed_on"`
				DueOn      string `json:"due_on"`
				Copy       struct {
					Book struct {
						Title string `json:"title"`
					} `json:"book"`
				} `json:"copy"`
			} `json:"loan"`
		} `json:"data"`
	}
	if err := json.Unmarshal(body, &answer); err != nil {
		return receiptLoan{}, fmt.Errorf("decoding the loan: %w", err)
	}
	if len(answer.Data.Loan) == 0 {
		// Either it does not exist or this member may not see it. The two are
		// deliberately the same answer.
		return receiptLoan{}, donat.Errorf("validation-failed", "no such loan")
	}
	found := answer.Data.Loan[0]
	return receiptLoan{
		ID:         found.ID,
		Title:      found.Copy.Book.Title,
		BorrowedOn: found.BorrowedOn,
		DueOn:      found.DueOn,
	}, nil
}

// receiptLoan is the little that a receipt needs to say.
type receiptLoan struct {
	ID         string
	Title      string
	BorrowedOn string
	DueOn      string
}

// renderReceiptPDF produces a minimal, valid PDF.
//
// A real service would reach for a typesetting library; the point of the
// example is where the bytes go, not how they are drawn, and a dependency-free
// renderer keeps that visible. The result opens in any reader.
func renderReceiptPDF(loan receiptLoan) []byte {
	lines := []string{
		"BORROWING RECEIPT",
		"",
		"Loan:        " + loan.ID,
		"Title:       " + loan.Title,
		"Borrowed on: " + loan.BorrowedOn,
		"Due on:      " + loan.DueOn,
	}

	var text bytes.Buffer
	text.WriteString("BT /F1 12 Tf 72 720 Td 16 TL\n")
	for _, line := range lines {
		fmt.Fprintf(&text, "(%s) Tj T*\n", escapePDFText(line))
	}
	text.WriteString("ET\n")

	objects := []string{
		"<< /Type /Catalog /Pages 2 0 R >>",
		"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
		"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] " +
			"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
		fmt.Sprintf("<< /Length %d >>\nstream\n%sendstream", text.Len(), text.String()),
		"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
	}

	var pdf bytes.Buffer
	pdf.WriteString("%PDF-1.4\n")
	offsets := make([]int, len(objects))
	for i, object := range objects {
		offsets[i] = pdf.Len()
		fmt.Fprintf(&pdf, "%d 0 obj\n%s\nendobj\n", i+1, object)
	}

	// The cross-reference table is what makes it a PDF rather than a text file
	// with a header: every offset must be exact, so they are recorded above as
	// each object is written.
	xref := pdf.Len()
	fmt.Fprintf(&pdf, "xref\n0 %d\n0000000000 65535 f \n", len(objects)+1)
	for _, offset := range offsets {
		fmt.Fprintf(&pdf, "%010d 00000 n \n", offset)
	}
	fmt.Fprintf(&pdf, "trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n",
		len(objects)+1, xref)
	return pdf.Bytes()
}

// escapePDFText escapes the three characters that are syntax inside a PDF
// string literal. A title containing a bracket would otherwise produce a file
// no reader will open.
func escapePDFText(s string) string {
	return strings.NewReplacer(`\`, `\\`, "(", `\(`, ")", `\)`).Replace(s)
}

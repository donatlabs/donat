package donat

import "fmt"

// FunctionError is an error a function returns when it wants to choose the
// code the caller sees.
//
// Without it every refusal is `unexpected`, which tells a client nothing it
// can act on: "this invoice has no lines" and "the PDF service is down" are
// different answers and deserve different codes. The message is the caller's,
// so it must not carry anything the caller may not see.
type FunctionError struct {
	// Code is the Donat error code, e.g. "validation-failed" for a refusal the
	// caller could fix, or "unexpected" for a fault they cannot.
	Code    string
	Message string
}

func (e *FunctionError) Error() string { return e.Message }

// Errorf returns a FunctionError carrying code.
//
//	return Out{}, donat.Errorf("validation-failed", "invoice %s has no lines", id)
func Errorf(code, format string, a ...any) error {
	return &FunctionError{Code: code, Message: fmt.Sprintf(format, a...)}
}

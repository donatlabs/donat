module github.com/donatlabs/donat/examples/petshop-golang

go 1.22

require (
	github.com/donatlabs/donat/sdk/go v0.0.0
	github.com/shopspring/decimal v1.4.0
)

// Use the in-repo SDK. Remove this once the SDK module is published/tagged
// and depend on a real version instead.
replace github.com/donatlabs/donat/sdk/go => ../../sdk/go

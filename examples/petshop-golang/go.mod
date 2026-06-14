module github.com/donatlabs/donat/examples/petshop-golang

go 1.22.0

require (
	github.com/donatlabs/donat/sdk/go v0.0.0
	github.com/jackc/pgx/v5 v5.7.4
	github.com/shopspring/decimal v1.4.0
)

require (
	github.com/jackc/pgpassfile v1.0.0 // indirect
	github.com/jackc/pgservicefile v0.0.0-20240606120523-5a60cdf6a761 // indirect
	github.com/jackc/puddle/v2 v2.2.2 // indirect
	github.com/tetratelabs/wazero v1.9.0 // indirect
	golang.org/x/crypto v0.31.0 // indirect
	golang.org/x/sync v0.10.0 // indirect
	golang.org/x/text v0.21.0 // indirect
)

// Use the in-repo SDK. Remove this once the SDK module is published/tagged
// and depend on a real version instead.
replace github.com/donatlabs/donat/sdk/go => ../../sdk/go

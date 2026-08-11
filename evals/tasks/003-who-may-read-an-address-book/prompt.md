# The address book nobody can reach

The Petshop stores a delivery address book. The table is in the database, with
its columns and its foreign key to the customer, and the application does not
expose it at all: no shopper can add an address, no one in support can look one
up, and nothing in the API knows the table is there.

Expose it.

## What already exists

- The table `public.customer_address` exists in the database, with these
  columns and no others:

  ```
  id            bigserial, the key
  customer_id   text, the customer who owns the row
  label         text, what the shopper calls this address
  line1         text
  line2         text, the only nullable column here
  city          text
  postal_code   text
  country_code  text, two letters — the database already refuses other shapes
  ```

  They are spelled out because the migrations are not in your workspace and
  nothing else in the metadata names them. Getting a column name wrong is a
  failure to guess, not a failure to design, and this task is not asking you to
  guess.
- Every other table in the store is exposed the same way, and those
  declarations are next to the one you are writing. Read two or three of them
  before you start: the conventions for roles, row visibility and writable
  columns are all there.
- The customer table declares a relationship to this one, so once the table is
  exposed, a shopper's addresses hang off their own record.

## Who these people are

- **A shopper** — the `customer` role. Signed in, and identified to the store
  by their own user id. There are many of them and they are strangers to each
  other.
- **Support** — the `support` role. Answers "where is my parcel" and needs to
  see the address a delivery is going to, whoever it belongs to.
- **A visitor** — the `anonymous` role. Not signed in.

## What each of them must be able to do

- A shopper keeps their own address book: adds an address, corrects one, and
  removes one they no longer use.
- A shopper reads their own addresses and no one else's. Not by convention —
  the store must not be able to hand them somebody else's row at all.
- Support reads any shopper's addresses, because that is the job.
- A visitor reads nothing here.

## The rules the business will hold you to

1. **An address belongs to the shopper who added it.** Not to whoever the
   request says it belongs to. A shopper who submits an address claiming
   another customer's id has added an address to *their own* book, or been
   refused — the one thing that must not happen is a row appearing in a
   stranger's book.
2. **Reading and writing are the same boundary.** A shopper who cannot read
   another's address must not be able to edit or delete it either. A row filter
   on the way out and no filter on the way in is not a boundary.
3. **A shopper changes only what is theirs to change.** The label and the
   address itself, yes. Which customer the row belongs to, no.
4. **Support looks, and that is all this task asks of support.** Whether
   support may also edit is a decision the store has already made elsewhere for
   comparable tables; follow what the store does.
5. **What the database guarantees is not what the application guarantees.** The
   country-code shape is a database check because a malformed code is wrong for
   every writer. Rules that exist because a carrier label has to print are the
   application's, and belong where the store puts that kind of rule.

## How this will be judged

By running the store. Two shoppers each keep an address book, support looks
things up, and a visitor tries. Nothing checks the shape of your YAML; what is
checked is who can see and change what, through the API, as those four kinds of
caller.

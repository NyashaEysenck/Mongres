const database = db.getSiblingDB("demo");

database.customers.insertMany([
  {
    _id: "customer-001",
    name: "Amina Ndlovu",
    active: true,
    profile: {
      address: {
        city: "Harare",
        country: "Zimbabwe",
      },
    },
  },
  {
    _id: "customer-002",
    name: "Tendai Moyo",
    active: false,
    profile: {
      address: {
        country: "Zimbabwe",
      },
    },
  },
]);

database.ambiguous_profiles.insertMany([
  {
    _id: "ambiguity-001",
    status: "active",
    profile: { city: "Harare" },
  },
  {
    _id: "ambiguity-002",
    status: 1,
    "profile.city": "literal-field-value",
  },
  {
    _id: "ambiguity-003",
    status: ["active"],
  },
]);

// Deliberately mixed scalar BSON types only. This is the bounded Phase-C
// fixture: an LLM may choose one Rust-generated candidate for a string SQL
// literal, while Rust performs the selected deterministic conversion.
database.mixed_statuses.insertMany([
  { _id: "status-001", status: "active" },
  { _id: "status-002", status: 1 },
]);

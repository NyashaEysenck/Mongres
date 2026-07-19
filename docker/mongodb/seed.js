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
        city: "Bulawayo",
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


import { ApolloServer } from "@apollo/server"; import { startStandaloneServer } from "@apollo/server/standalone";
const s = new ApolloServer({ typeDefs: "type Query { hi: String }", resolvers: { Query: { hi: () => "gql" } } });
const { url } = await startStandaloneServer(s, { listen: { port: 0 } });
const j = await (await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ query: "{hi}" }) })).json();
await s.stop(); if (j.data.hi !== "gql") throw new Error(JSON.stringify(j)); console.log("SMOKE_OK");

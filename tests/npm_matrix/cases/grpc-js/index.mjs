import grpc from "@grpc/grpc-js"; import loader from "@grpc/proto-loader";
const def = grpc.loadPackageDefinition(loader.loadSync("h.proto")).h; const srv = new grpc.Server();
srv.addService(def.G.service, { Hi: (c, cb) => cb(null, { m: "hi " + c.request.n }) });
srv.bindAsync("127.0.0.1:0", grpc.ServerCredentials.createInsecure(), (e, port) => { const cl = new def.G(`127.0.0.1:${port}`, grpc.credentials.createInsecure());
  cl.Hi({ n: "g" }, (err, r) => { if (err || r.m !== "hi g") { console.error(err); process.exit(1); } cl.close(); srv.forceShutdown(); console.log("SMOKE_OK"); }); });

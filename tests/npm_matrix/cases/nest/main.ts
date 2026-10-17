import "reflect-metadata";
import { Controller, Get, Injectable, Module } from "@nestjs/common";
import { NestFactory } from "@nestjs/core";
@Injectable() class Svc { hi() { return { hi: "nest" }; } }
@Controller() class Ctl { constructor(private readonly s: Svc) {} @Get() get() { return this.s.hi(); } }
@Module({ controllers: [Ctl], providers: [Svc] }) class AppModule {}
const app = await NestFactory.create(AppModule, { logger: false });
await app.listen(0, "127.0.0.1");
const port = (app.getHttpServer().address() as any).port;
const j = await (await fetch(`http://127.0.0.1:${port}/`)).json();
if (j.hi !== "nest") throw new Error("nest DI failed: " + JSON.stringify(j));
await app.close(); console.log("SMOKE_OK");

import { S3Client, CreateBucketCommand, PutObjectCommand, GetObjectCommand, DeleteObjectCommand, DeleteBucketCommand } from "@aws-sdk/client-s3";
const c = new S3Client({ endpoint: "http://127.0.0.1:59000", region: "us-east-1", forcePathStyle: true, credentials: { accessKeyId: "minioadmin", secretAccessKey: "minioadmin" } });
const B = "ncp" + Date.now(); await c.send(new CreateBucketCommand({ Bucket: B })); await c.send(new PutObjectCommand({ Bucket: B, Key: "k", Body: "s3data" }));
const o = await c.send(new GetObjectCommand({ Bucket: B, Key: "k" })); const t = await o.Body.transformToString();
await c.send(new DeleteObjectCommand({ Bucket: B, Key: "k" })); await c.send(new DeleteBucketCommand({ Bucket: B }));
if (t !== "s3data") throw new Error("s3"); console.log("SMOKE_OK");

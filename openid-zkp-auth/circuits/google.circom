pragma circom 2.0.0;

include "jwt_proof.circom";

component main {
    public [jwt_sha2_hash, masked_content_hash, payload_index, 
            eph_public_key, max_epoch, nonce]
} = JwtProof(
    704,
    [
        [76,67,74,122,100,87,73,105,79,105,73,120,77,84,65,48,78,106,77,
         48,78,84,73,120,78,106,99,122,77,68,77,49,79,84,103,122,79,68,77,105],
        [119,105,99,51,86,105,73,106,111,105,77,84,69,119,78,68,89,122,
         78,68,85,121,77,84,89,51,77,122,65,122,78,84,107,52,77,122,103,122,73,105],
        [73,110,78,49,89,105,73,54,73,106,69,120,77,68,81,50,77,122,81,49,77,106,69,50,
         78,122,77,119,77,122,85,53,79,68,77,52,77,121,73,115]
    ],
    40,
    [0,2,0]
);
